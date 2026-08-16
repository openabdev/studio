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

/// One agent endpoint in the registry (`[[agent]]` in `agents.toml`): a named
/// `/acp` connection plus a `management` policy flag. The connection fields are
/// the same shape as [`RemoteConfig`] (a legacy `remote.toml` maps to a single
/// `management = true` entry), so the dial path and validation are shared.
///
/// Fields are spelled out rather than `#[serde(flatten)]`-ing a `RemoteConfig`
/// because `toml` serialization of a flattened struct is order-fragile; the
/// [`AgentEndpoint::conn`] accessor rebuilds the `RemoteConfig` the transport
/// dials with, keeping one source of truth for connection validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEndpoint {
    /// Stable identity — the map key in `RemoteState` and the label in the
    /// selector. Required (a nameless entry can't be addressed).
    #[serde(default)]
    pub name: String,
    /// The `/acp` WSS endpoint.
    #[serde(default)]
    pub url: String,
    /// The `/acp` bearer token (secret; never logged).
    #[serde(default)]
    pub token: String,
    /// The session `cwd` sent on `session/new` (defaults to `/`).
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// This entry backs the **management console**: Studio publishes its
    /// reverse-MCP `oab` fleet-control tools to this agent (the whole point of
    /// the management binding). **Off by default** — an ordinary agent console
    /// chats with and configures an agent *without* granting it fleet control
    /// (least privilege, ADR agent-consoles Part A).
    #[serde(default)]
    pub management: bool,
}

impl Default for AgentEndpoint {
    fn default() -> Self {
        AgentEndpoint {
            name: String::new(),
            url: String::new(),
            token: String::new(),
            cwd: default_cwd(),
            management: false,
        }
    }
}

impl AgentEndpoint {
    /// The connection view the transport dials with — one source of truth for
    /// the `/acp` handshake fields and their validation.
    pub fn conn(&self) -> RemoteConfig {
        RemoteConfig {
            url: self.url.clone(),
            token: self.token.clone(),
            cwd: self.cwd.clone(),
        }
    }

    /// Enough is set to attempt a connection (a name **and** a configured conn).
    pub fn is_configured(&self) -> bool {
        !self.name.trim().is_empty() && self.conn().is_configured()
    }

    /// Validate before dialing: a name is required (it addresses the endpoint),
    /// then the connection fields validate as a [`RemoteConfig`].
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("agent name is required (it addresses the endpoint)".to_string());
        }
        self.conn().validate()
    }
}

/// The per-agent endpoint registry (`~/.config/oab-studio/agents.toml`): a list
/// of `[[agent]]` entries. Generalizes the single `remote.toml` so Studio can
/// reach N agents — one carries `management = true` (the management console + its
/// reverse-MCP grant); all are selectable as agent consoles. A legacy
/// `remote.toml` is adopted via [`AgentRegistry::from_legacy`] as one management
/// entry, so existing setups keep working (ADR agent-consoles Part B).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentRegistry {
    /// The endpoints, one per `[[agent]]` table. Empty ⇒ "not configured".
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentEndpoint>,
}

impl AgentRegistry {
    /// Parse `agents.toml`. An empty file yields an empty registry (not an error),
    /// so a fresh install is "not configured" rather than broken.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Serialize back to `agents.toml` text (used when the app writes structured
    /// changes; the editor otherwise round-trips the operator's raw text).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Adopt a legacy single `remote.toml` as one `management = true` entry, so a
    /// pre-registry setup keeps working while the registry is rolled out. The
    /// entry is named `name` (the app passes a stable default like `"management"`).
    pub fn from_legacy(cfg: RemoteConfig, name: &str) -> Self {
        AgentRegistry {
            agents: vec![AgentEndpoint {
                name: name.to_string(),
                url: cfg.url,
                token: cfg.token,
                cwd: cfg.cwd,
                management: true,
            }],
        }
    }

    /// Look an endpoint up by name (the `RemoteState` key).
    pub fn get(&self, name: &str) -> Option<&AgentEndpoint> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// The management endpoint (the one carrying `management = true`), if any.
    /// Backs the top-level console and its reverse-MCP `oab` grant; also the
    /// default target for the legacy single-endpoint commands.
    pub fn management(&self) -> Option<&AgentEndpoint> {
        self.agents.iter().find(|a| a.management)
    }

    /// No endpoints configured.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Structural validation for a file save (mirrors how the fleets.toml editor
    /// rejects a bad file without writing): every entry is named, names are
    /// unique (they key connections), and **at most one** entry is `management`
    /// (exactly one binding carries the reverse-MCP grant). Per-endpoint
    /// connection completeness is checked at dial time, not here — so a
    /// half-filled entry can still be saved, like an empty `remote.toml`.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        let mut management = 0usize;
        for a in &self.agents {
            let name = a.name.trim();
            if name.is_empty() {
                return Err("every [[agent]] needs a name".to_string());
            }
            if !seen.insert(name) {
                return Err(format!("duplicate agent name {name:?} — names must be unique"));
            }
            if a.management {
                management += 1;
            }
        }
        if management > 1 {
            return Err("at most one agent may be management = true".to_string());
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

    // ---- AgentRegistry (per-agent endpoint registry) --------------------------

    #[test]
    fn registry_parses_multiple_agents() {
        let reg = AgentRegistry::parse(
            r#"
[[agent]]
name = "orca"
url = "wss://orca/acp"
token = "s1"
cwd = "/home/node"
management = true

[[agent]]
name = "mira"
url = "wss://mira/acp"
token = "s2"
"#,
        )
        .expect("parse");
        assert_eq!(reg.agents.len(), 2);
        let orca = reg.get("orca").expect("orca present");
        assert_eq!(orca.cwd, "/home/node");
        assert!(orca.management);
        assert!(orca.is_configured());
        // cwd defaults to "/" when omitted, mirroring RemoteConfig.
        let mira = reg.get("mira").expect("mira present");
        assert_eq!(mira.cwd, "/");
        assert!(!mira.management); // management defaults off (least privilege)
        // management() returns the one flagged entry.
        assert_eq!(reg.management().map(|a| a.name.as_str()), Some("orca"));
    }

    #[test]
    fn empty_registry_is_not_configured() {
        let reg = AgentRegistry::parse("").expect("empty parses");
        assert!(reg.is_empty());
        assert!(reg.management().is_none());
        assert_eq!(reg, AgentRegistry::default());
        assert!(reg.validate().is_ok());
    }

    #[test]
    fn from_legacy_remote_becomes_one_management_entry() {
        let cfg = RemoteConfig {
            url: "wss://gw/acp".into(),
            token: "sek".into(),
            cwd: "/work".into(),
        };
        let reg = AgentRegistry::from_legacy(cfg, "management");
        assert_eq!(reg.agents.len(), 1);
        let e = reg.management().expect("has management");
        assert_eq!(e.name, "management");
        assert_eq!(e.cwd, "/work");
        assert!(e.management);
        // The adopted entry dials with the same conn the legacy file did.
        assert!(e.conn().validate().is_ok());
    }

    #[test]
    fn validate_rejects_dup_names_missing_names_and_two_managements() {
        assert!(AgentRegistry {
            agents: vec![AgentEndpoint { name: "a".into(), ..Default::default() },
                         AgentEndpoint { name: "a".into(), ..Default::default() }],
        }
        .validate()
        .unwrap_err()
        .contains("unique"));

        assert!(AgentRegistry {
            agents: vec![AgentEndpoint { name: "  ".into(), ..Default::default() }],
        }
        .validate()
        .is_err());

        assert!(AgentRegistry {
            agents: vec![
                AgentEndpoint { name: "a".into(), management: true, ..Default::default() },
                AgentEndpoint { name: "b".into(), management: true, ..Default::default() },
            ],
        }
        .validate()
        .unwrap_err()
        .contains("management"));
    }

    #[test]
    fn endpoint_validate_requires_name_and_conn() {
        // Missing name → error even with a good conn.
        assert!(AgentEndpoint {
            name: "".into(),
            url: "wss://x/acp".into(),
            token: "t".into(),
            ..Default::default()
        }
        .validate()
        .unwrap_err()
        .contains("name"));
        // Named but unconfigured conn → the RemoteConfig validation fires.
        assert!(AgentEndpoint { name: "x".into(), ..Default::default() }
            .validate()
            .is_err());
    }

    #[test]
    fn registry_to_toml_round_trips() {
        let reg = AgentRegistry {
            agents: vec![
                AgentEndpoint {
                    name: "orca".into(),
                    url: "wss://orca/acp".into(),
                    token: "s1".into(),
                    cwd: "/home/node".into(),
                    management: true,
                },
                AgentEndpoint {
                    name: "mira".into(),
                    url: "wss://mira/acp".into(),
                    token: "s2".into(),
                    cwd: "/".into(),
                    management: false,
                },
            ],
        };
        assert_eq!(AgentRegistry::parse(&reg.to_toml()).unwrap(), reg);
    }
}
