//! Vendor image tag resolution (studio#128): resolves a vendor name (e.g.
//! `claude`, `codex`, `cursor`, `kiro`, `antigravity`) to real,
//! currently-published `ghcr.io/openabdev/openab` image tags — "Beta" (the
//! hourly rolling `pre-beta-<vendor>` build) and "Stable" (the newest
//! openab release whose matching `<version>-<vendor>` image is confirmed to
//! actually exist).
//!
//! A GitHub release tag existing does **not** guarantee a matching image
//! was ever published: the image-build workflow (`build-images.yml`) is a
//! manual `workflow_dispatch` step, completely disconnected from cutting a
//! release — confirmed by reading both workflows. So "stable" has to be
//! verified against GHCR directly, not inferred from the release list
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

/// Real (non-beta) openab release version numbers, newest first — matches
/// GitHub's own default ordering for this endpoint. Filters on both the
/// `prerelease` flag *and* the tag name itself: at least one real release
/// (`openab-0.10.0-beta.3`) has `prerelease: false` despite its name, so
/// the flag alone isn't reliable.
async fn openab_release_versions(client: &reqwest::Client) -> Result<Vec<String>> {
    let releases: Vec<GhRelease> = client
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
        .context("GitHub releases API returned invalid JSON")?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.prerelease && r.tag_name.starts_with("openab-") && !r.tag_name.contains("-beta"))
        .map(|r| r.tag_name.trim_start_matches("openab-").to_string())
        .collect())
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VendorImageTags {
    /// `pre-beta-<vendor>` if the GHCR check confirms it exists.
    pub beta: Option<String>,
    /// The newest release version whose `<version>-<vendor>` image is
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

    let beta_tag = format!("pre-beta-{vendor}");
    if ghcr_tag_exists(&client, &token, &beta_tag).await.unwrap_or(false) {
        out.beta = Some(beta_tag);
    }

    if let Ok(versions) = openab_release_versions(&client).await {
        for version in versions {
            let candidate = format!("{version}-{vendor}");
            if ghcr_tag_exists(&client, &token, &candidate).await.unwrap_or(false) {
                out.stable = Some(candidate);
                break;
            }
        }
    }

    out
}
