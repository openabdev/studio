//! Local AWS credential-profile discovery (studio#104) — the AWS half of the
//! "+ New fleet" wizard's provider `<select>` fields.
//!
//! Lives in its own module because the code is pure-std (filesystem + env
//! only, no kube/AWS-SDK types): `crates/oabctl/tests/` can `#[path]`-include
//! this file directly and exercise the real parse/scan path without a
//! dependency edge — the workspace's verified test entry point is
//! `cargo test --package oabctl`, so cross-crate coverage has to be reachable
//! from there. (The kubeconfig half of discovery can't do that — it needs
//! `kube`/`gcp_auth` — so it stays in `lib.rs`.)
//!
//! Backs the wizard's Credential profile `<select>` — the desktop app spawns
//! `oab-mcp` as a local sidecar (`src-tauri/src/mcp.rs`), so this reads the
//! *operator's own machine*, not a remote server. Deliberately hand-rolled
//! (not `aws-config`'s internal profile-file parser, which isn't a stable
//! public surface) — same "pure, regex/line-based, easy to unit-test" spirit
//! as `fleetToml.ts`'s client-side TOML edits.

/// One AWS credential profile discovered on the local machine.
pub struct AwsProfile {
    pub name: String,
    pub region: Option<String>,
}

/// Result of scanning `~/.aws/config` (+ `~/.aws/credentials` for profiles that
/// only exist there). `exists=false` means neither file was found — the caller
/// (console) shows "run `aws configure`" guidance rather than a bare error.
/// `error` is set only when a file exists but couldn't be read/parsed.
pub struct AwsProfilesResult {
    pub profiles: Vec<AwsProfile>,
    pub source_path: String,
    pub exists: bool,
    pub error: Option<String>,
}

fn aws_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".aws"))
}

/// Parse `~/.aws/config`'s `[default]` / `[profile <name>]` sections, pulling
/// out `region` when present. Comments (`#`/`;`) and blank lines are ignored;
/// unrecognized keys are skipped (this isn't a general INI parser, just enough
/// to answer "what profiles exist, with what region").
fn parse_aws_config(text: &str) -> Vec<AwsProfile> {
    let mut profiles = Vec::new();
    let mut current: Option<AwsProfile> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if let Some(p) = current.take() {
                profiles.push(p);
            }
            // Only `[default]` and `[profile <name>]` sections are credential
            // profiles — `[sso-session …]`/`[services …]`/`[preview]`/
            // `[plugins …]` configure other things and must not be offered as
            // profiles by `list_aws_profiles` (studio#104).
            let name = if header == "default" {
                Some("default")
            } else {
                header.strip_prefix("profile ")
            };
            current = name.map(|n| AwsProfile {
                name: n.trim().to_string(),
                region: None,
            });
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "region" {
                if let Some(p) = current.as_mut() {
                    p.region = Some(value.trim().to_string());
                }
            }
        }
    }
    if let Some(p) = current.take() {
        profiles.push(p);
    }
    profiles
}

/// Profile names from `~/.aws/credentials`'s `[<name>]` sections (bare, no
/// `profile ` prefix there) — covers profiles that only carry credentials with
/// no matching `~/.aws/config` entry (no region info available for these).
fn parse_aws_credentials_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|raw_line| {
            let line = raw_line.trim();
            line.strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .map(|name| name.trim().to_string())
        })
        .collect()
}

/// List AWS profiles discoverable on this machine, merging `~/.aws/config`
/// (name + region) with any profile-only-in-`~/.aws/credentials` names.
pub fn list_aws_profiles() -> AwsProfilesResult {
    let Some(dir) = aws_dir() else {
        return AwsProfilesResult {
            profiles: Vec::new(),
            source_path: String::new(),
            exists: false,
            error: None,
        };
    };
    let config_path = dir.join("config");
    let source_path = config_path.display().to_string();

    let mut profiles = match std::fs::read_to_string(&config_path) {
        Ok(text) => parse_aws_config(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return AwsProfilesResult {
                profiles: Vec::new(),
                source_path,
                exists: true,
                error: Some(e.to_string()),
            };
        }
    };

    let mut error = None;
    match std::fs::read_to_string(dir.join("credentials")) {
        Ok(creds_text) => {
            for name in parse_aws_credentials_names(&creds_text) {
                if !profiles.iter().any(|p| p.name == name) {
                    profiles.push(AwsProfile { name, region: None });
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // Same tier contract as the config read above: a file that exists but
        // can't be read is a real error worth showing, not "nothing
        // configured" — otherwise the console's missing-config guidance
        // tells the operator to `aws configure` over a file they already have.
        Err(e) => error = Some(e.to_string()),
    }

    let exists = config_path.exists() || dir.join("credentials").exists();
    AwsProfilesResult {
        profiles,
        source_path,
        exists,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_config_parses_default_and_named_profiles_with_region() {
        let text = "\
[default]
region = us-east-1
output = json

[profile oab-fleet]
region = ap-east-2

[profile no-region]
";
        let profiles = parse_aws_config(text);
        assert_eq!(profiles.len(), 3);
        assert_eq!(profiles[0].name, "default");
        assert_eq!(profiles[0].region.as_deref(), Some("us-east-1"));
        assert_eq!(profiles[1].name, "oab-fleet");
        assert_eq!(profiles[1].region.as_deref(), Some("ap-east-2"));
        assert_eq!(profiles[2].name, "no-region");
        assert_eq!(profiles[2].region, None);
    }

    #[test]
    fn aws_config_ignores_comments_and_blank_lines() {
        let text = "\
# a comment
; also a comment

[default]
; region is commented out
# region = eu-west-1
region = us-west-2
";
        let profiles = parse_aws_config(text);
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn aws_config_empty_text_yields_no_profiles() {
        assert!(parse_aws_config("").is_empty());
    }

    #[test]
    fn aws_credentials_names_are_bare_no_profile_prefix() {
        let text = "\
[default]
aws_access_key_id = AKIA...

[oab-fleet]
aws_access_key_id = AKIA...
";
        let names = parse_aws_credentials_names(text);
        assert_eq!(names, vec!["default".to_string(), "oab-fleet".to_string()]);
    }
}
