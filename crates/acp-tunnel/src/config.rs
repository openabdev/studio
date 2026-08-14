//! The reverse-MCP **remote-connection config** — the `/acp` endpoint Studio
//! dials to attach to an agent runtime (reverse-MCP client ADR §5).
//!
//! Stored as a TOML file the operator can **edit in-app** (the same editor
//! pattern as `fleets.toml`), at `~/.config/oab-studio/remote.toml`. Parsing,
//! serialization, and validation are pure (here); the file IO + the actual dial
//! live in the transport (`src-tauri`).
//!
//! ⚠️ The `token` is a `/acp` bearer — a **secret** that lives in this file. The
//! app surfaces it in the editor (like any editable config) but must never log
//! it. Prefer the loopback/keyless posture when the gateway allows it.

use serde::{Deserialize, Serialize};

fn default_cwd() -> String {
    "/".to_string()
}

/// The declarative remote-connection config. All fields default to empty so a
/// partial / not-yet-configured file still parses (the app shows it as
/// "not configured" rather than erroring).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// The `/acp` WSS endpoint (e.g. `wss://gateway.example/acp`).
    #[serde(default)]
    pub url: String,
    /// The `/acp` bearer token (secret; rides the WS sub-protocol on dial).
    #[serde(default)]
    pub token: String,
    /// The session `cwd` sent on `session/new` (defaults to `/`).
    #[serde(default = "default_cwd")]
    pub cwd: String,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        RemoteConfig {
            url: String::new(),
            token: String::new(),
            cwd: default_cwd(),
        }
    }
}

impl RemoteConfig {
    /// Parse from the TOML file text. An empty file yields the default (empty)
    /// config rather than an error, so a fresh install is "not configured".
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Serialize to TOML for writing the file back (round-trips the fields; the
    /// editor holds the operator's raw text, so this is only used when the app
    /// writes structured changes rather than raw text).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Whether enough is set to attempt a connection: a URL **and** a token.
    /// (Drives whether the "Activate" button is enabled.)
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.token.trim().is_empty()
    }

    /// Validate before dialing. Checks the URL is present and a WebSocket scheme;
    /// a bad edit is rejected **without** connecting (mirroring how the fleet
    /// config editor rejects bad TOML without writing). The token's validity is
    /// only knowable at the handshake, so it is not checked here beyond presence.
    pub fn validate(&self) -> Result<(), String> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err("url is required (the /acp WSS endpoint)".to_string());
        }
        if !(url.starts_with("ws://") || url.starts_with("wss://")) {
            return Err(format!(
                "url must be a WebSocket endpoint (ws:// or wss://), got {url:?}"
            ));
        }
        if self.token.trim().is_empty() {
            return Err("token is required (the /acp bearer)".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_config() {
        let cfg = RemoteConfig::parse(
            r#"
url = "wss://gw.example/acp"
token = "sekret"
cwd = "/work"
"#,
        )
        .expect("parse");
        assert_eq!(cfg.url, "wss://gw.example/acp");
        assert_eq!(cfg.token, "sekret");
        assert_eq!(cfg.cwd, "/work");
        assert!(cfg.is_configured());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn empty_file_is_the_default_not_configured() {
        let cfg = RemoteConfig::parse("").expect("empty parses");
        assert_eq!(cfg, RemoteConfig::default());
        assert_eq!(cfg.cwd, "/"); // cwd defaults even when absent
        assert!(!cfg.is_configured());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn cwd_defaults_when_omitted() {
        let cfg = RemoteConfig::parse(r#"url = "wss://x/acp"
token = "t""#)
            .expect("parse");
        assert_eq!(cfg.cwd, "/");
    }

    #[test]
    fn validate_rejects_missing_url_bad_scheme_and_missing_token() {
        assert!(RemoteConfig { url: "".into(), token: "t".into(), cwd: "/".into() }
            .validate()
            .is_err());
        assert!(RemoteConfig {
            url: "http://x/acp".into(),
            token: "t".into(),
            cwd: "/".into()
        }
        .validate()
        .unwrap_err()
        .contains("WebSocket"));
        assert!(RemoteConfig {
            url: "wss://x/acp".into(),
            token: "".into(),
            cwd: "/".into()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn to_toml_round_trips() {
        let cfg = RemoteConfig {
            url: "wss://x/acp".into(),
            token: "t".into(),
            cwd: "/w".into(),
        };
        assert_eq!(RemoteConfig::parse(&cfg.to_toml()).unwrap(), cfg);
    }
}
