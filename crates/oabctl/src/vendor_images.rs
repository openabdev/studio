//! Vendor image tag resolution (studio#128): resolves a vendor name (e.g.
//! `claude`, `codex`, `cursor`, `kiro`, `antigravity`) to real,
//! currently-published `ghcr.io/openabdev/openab` image tags — "Beta" (the
//! newest beta-named openab release, `<version>-beta.N`, whose matching
//! `<version>-beta.N-<vendor>` image is confirmed to exist) and "Stable"
//! (the newest non-beta release whose matching `<version>-<vendor>` image
//! is confirmed to exist). Both are pinned version tags, never the rolling
//! `pre-beta-<vendor>`/`nightly-<vendor>` moving tags — a caller (or a
//! human reading the dropdown) needs an actual version number to reason
//! about "is this new enough for fix X", which a moving tag can't answer.
//!
//! A GitHub release tag existing does **not** guarantee a matching image
//! was ever published: the image-build workflow (`build-images.yml`) is a
//! manual `workflow_dispatch` step, completely disconnected from cutting a
//! release — confirmed by reading both workflows. So both channels have to
//! be verified against GHCR directly, not inferred from the release list
//! alone.
//!
//! All access here is anonymous — no GitHub token needed. `ghcr.io` speaks
//! the standard OCI Distribution API, and `openabdev/openab` is a public
//! package: `GET /token?scope=repository:<repo>:pull` mints a scoped
//! anonymous pull token, the same flow `docker pull` uses against a public
//! image with no login. This is a different API from GitHub's Packages
//! REST API (`/orgs/.../packages/...`), which *does* require a
//! `read:packages`-scoped token even for public packages — deliberately
//! not used here for that reason.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const GHCR_REPO: &str = "openabdev/openab";

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
}

async fn ghcr_pull_token(client: &reqwest::Client) -> Result<String> {
    let url = format!("https://ghcr.io/token?scope=repository:{GHCR_REPO}:pull");
    let resp: TokenResponse = client
        .get(&url)
        .send()
        .await
        .context("failed to reach ghcr.io token endpoint")?
        .error_for_status()
        .context("ghcr.io token endpoint returned an error")?
        .json()
        .await
        .context("ghcr.io token endpoint returned invalid JSON")?;
    Ok(resp.token)
}

/// Does `ghcr.io/openabdev/openab:<tag>` actually exist? A manifest `HEAD`,
/// not a full pull — no image bytes transferred, just an existence check.
async fn ghcr_tag_exists(client: &reqwest::Client, token: &str, tag: &str) -> Result<bool> {
    let url = format!("https://ghcr.io/v2/{GHCR_REPO}/manifests/{tag}");
    let resp = client
        .head(&url)
        .bearer_auth(token)
        .header(
            "Accept",
            "application/vnd.oci.image.index.v1+json, \
             application/vnd.docker.distribution.manifest.list.v2+json, \
             application/vnd.oci.image.manifest.v1+json, \
             application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await
        .with_context(|| format!("failed to check ghcr.io tag '{tag}'"))?;
    Ok(resp.status().is_success())
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    prerelease: bool,
}

/// One GitHub API call, split into the two channels `resolve_vendor_image_tags`
/// walks — newest first in both, matching GitHub's own default ordering for
/// this endpoint. `prerelease` alone isn't a reliable channel signal (at
/// least one real release, `openab-0.10.0-beta.3`, has `prerelease: false`
/// despite its name) — the tag name itself decides stable vs. beta.
async fn fetch_openab_releases(client: &reqwest::Client) -> Result<Vec<GhRelease>> {
    client
        .get("https://api.github.com/repos/openabdev/openab/releases")
        // GitHub's REST API rejects requests with no User-Agent.
        .header("User-Agent", "openab-studio")
        .send()
        .await
        .context("failed to reach GitHub releases API")?
        .error_for_status()
        .context("GitHub releases API returned an error")?
        .json()
        .await
        .context("GitHub releases API returned invalid JSON")
}

/// Real (non-beta) openab release version numbers, newest first.
fn stable_release_versions(releases: &[GhRelease]) -> Vec<String> {
    releases
        .iter()
        .filter(|r| !r.prerelease && r.tag_name.starts_with("openab-") && !r.tag_name.contains("-beta"))
        .map(|r| r.tag_name.trim_start_matches("openab-").to_string())
        .collect()
}

/// Beta-named openab release version numbers (`<version>-beta.N`), newest
/// first — same "trust the tag name, not the `prerelease` flag" reasoning
/// as `stable_release_versions`.
fn beta_release_versions(releases: &[GhRelease]) -> Vec<String> {
    releases
        .iter()
        .filter(|r| r.tag_name.starts_with("openab-") && r.tag_name.contains("-beta"))
        .map(|r| r.tag_name.trim_start_matches("openab-").to_string())
        .collect()
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VendorImageTags {
    /// The newest beta release version (`<version>-beta.N`) whose
    /// `<version>-beta.N-<vendor>` image is confirmed to exist on GHCR.
    /// Pinned, not the old rolling `pre-beta-<vendor>` moving tag — a
    /// caller needs a version number to reason about (e.g. "is this
    /// build new enough for fix X"), which a moving tag can't give.
    /// `None` if no beta release has a matching image yet (or the
    /// GitHub/GHCR calls themselves failed).
    pub beta: Option<String>,
    /// The newest stable release version whose `<version>-<vendor>` image is
    /// confirmed to exist on GHCR. `None` if no release has a matching
    /// image yet (or the GitHub/GHCR calls themselves failed).
    pub stable: Option<String>,
}

/// Resolves both channels for `vendor`. Never fails outright — a failed
/// GHCR/GitHub call just leaves the corresponding field (or both) `None`
/// rather than erroring the whole wizard; the console falls back to a
/// plain editable text field either way (Brett: "Image tag is allow to be
/// manually input by user").
pub async fn resolve_vendor_image_tags(vendor: &str) -> VendorImageTags {
    let client = reqwest::Client::new();
    let mut out = VendorImageTags::default();

    let Ok(token) = ghcr_pull_token(&client).await else {
        return out;
    };

    let Ok(releases) = fetch_openab_releases(&client).await else {
        return out;
    };

    for version in beta_release_versions(&releases) {
        let candidate = format!("{version}-{vendor}");
        if ghcr_tag_exists(&client, &token, &candidate).await.unwrap_or(false) {
            out.beta = Some(candidate);
            break;
        }
    }

    for version in stable_release_versions(&releases) {
        let candidate = format!("{version}-{vendor}");
        if ghcr_tag_exists(&client, &token, &candidate).await.unwrap_or(false) {
            out.stable = Some(candidate);
            break;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag_name: &str, prerelease: bool) -> GhRelease {
        GhRelease { tag_name: tag_name.to_string(), prerelease }
    }

    // Real-world snapshot (2026-09-08, `gh api repos/openabdev/openab/releases`):
    // newest-first, mixes stable/beta/unrelated tags, and includes the
    // `prerelease: false` beta release that makes the flag alone unreliable.
    fn sample_releases() -> Vec<GhRelease> {
        vec![
            release("oabctl-pre-beta", true),
            release("openab-0.10.0-beta.3", false),
            release("openab-0.10.0-beta.2", false),
            release("openab-0.10.0-beta.1", false),
            release("openab-0.9.0", false),
            release("openab-0.9.0-beta.12", false),
            release("pre-seed-utils-v2.35.13-ghp0.3.2", false),
        ]
    }

    #[test]
    fn stable_versions_excludes_beta_and_unrelated_tags() {
        assert_eq!(stable_release_versions(&sample_releases()), vec!["0.9.0"]);
    }

    #[test]
    fn beta_versions_newest_first_ignores_prerelease_flag() {
        assert_eq!(
            beta_release_versions(&sample_releases()),
            vec!["0.10.0-beta.3", "0.10.0-beta.2", "0.10.0-beta.1", "0.9.0-beta.12"]
        );
    }

    #[test]
    fn both_channels_ignore_non_openab_prefixed_releases() {
        let releases = vec![release("oabctl-pre-beta", true), release("pre-seed-utils-v2.35.13-ghp0.3.2", false)];
        assert!(stable_release_versions(&releases).is_empty());
        assert!(beta_release_versions(&releases).is_empty());
    }
}
