//! Repo-hygiene regression: the whole workspace stays `cargo fmt` clean.
//! `cargo fmt --all -- --check` is part of the repo verification suite; this
//! test fails fast when formatting drifts in any crate instead of letting it
//! accumulate silently.

use std::process::Command;

#[test]
fn workspace_is_rustfmt_clean() {
    let output = Command::new(env!("CARGO"))
        .args(["fmt", "--all", "--", "--check"])
        .output()
        .expect("spawn cargo fmt");
    assert!(
        output.status.success(),
        "cargo fmt --all -- --check failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
