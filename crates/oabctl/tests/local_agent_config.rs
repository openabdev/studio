//! Regression coverage for `oabctl::studio_api::write_local_agent_config`
//! (studio#135): the local "Config folder" mirror the New Fleet wizard's
//! `deploy_provision_agent` maintains — `<folder>/<name>/config.toml`, the
//! same layout src-tauri's `list_local_agent_configs` /
//! `read_local_agent_config` scan and the Debug drawer's "Agent configs"
//! tab reads.

use oabctl::studio_api::write_local_agent_config;
use std::path::PathBuf;

/// A fresh per-test directory under the OS temp dir — this crate has no
/// `tempfile` dev-dep, and a unique root per test keeps these safe to run in
/// parallel.
fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oabctl-local-agent-config-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp root");
    dir
}

#[test]
fn writes_config_toml_under_name_subdir() {
    let folder = fresh_dir("basic");
    write_local_agent_config(folder.to_str().unwrap(), "athena", b"[agent]\nname = \"a\"\n")
        .expect("write");
    // Byte-identical: the local copy is the clean, pre-`inject_pre_seed_hook`
    // text, not an artifact rewritten for the bundle carrier.
    let written = folder.join("athena").join("config.toml");
    assert_eq!(
        std::fs::read(&written).expect("config.toml written"),
        b"[agent]\nname = \"a\"\n"
    );
}

#[test]
fn creates_missing_folder_and_name_dir() {
    // A Config folder pointing at a path that doesn't exist yet is normal —
    // the setting is just a string, the user may not have created it.
    let root = fresh_dir("deep");
    let folder = root.join("nested").join("configs");
    write_local_agent_config(folder.to_str().unwrap(), "ares", b"x").expect("write");
    assert!(folder.join("ares").join("config.toml").is_file());
}

#[test]
fn refuses_names_that_escape_the_config_folder() {
    // `name` arrives straight from the `deploy_provision_agent` tool call —
    // any MCP caller can issue it, not just the wizard — so a `..`/separator
    // must never reach the filesystem join: it would write config.toml (and
    // mkdirs) outside the Config folder entirely.
    let folder = fresh_dir("traversal");
    for name in ["..", ".", "", "a/b", "../sibling", "/abs", "a\\b"] {
        assert!(
            write_local_agent_config(folder.to_str().unwrap(), name, b"x").is_err(),
            "name {name:?} must be refused"
        );
    }
    // Refusal happens before any fs mutation: nothing created inside the
    // folder, and no stray dirs beside it either.
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
}

#[test]
fn names_with_spaces_or_unicode_are_still_fine() {
    // The guard rejects path components, not "weird" names — an agent named
    // "Persephone 2" is a legal single directory and must keep working.
    let folder = fresh_dir("unicode");
    write_local_agent_config(folder.to_str().unwrap(), "Persephone 2", b"x").expect("write");
    assert!(folder.join("Persephone 2").join("config.toml").is_file());
}

#[test]
fn errors_when_folder_path_is_a_file() {
    let root = fresh_dir("file");
    let folder = root.join("blocked");
    std::fs::write(&folder, b"not a dir").unwrap();
    assert!(write_local_agent_config(folder.to_str().unwrap(), "athena", b"x").is_err());
}
