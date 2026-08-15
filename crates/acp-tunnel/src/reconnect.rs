//! Reconnect policy for the remote `/acp` client: how long to wait between
//! attempts, and how to classify *why* an attempt ended so the operator sees a
//! meaningful reason instead of a raw error string. Pure + unit-tested here so the
//! Tauri driver (`src-tauri/src/remote.rs`) stays thin.

use std::time::Duration;

/// Why a connection attempt ended — drives the operator-facing status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// DNS / TCP / TLS / timeout — the socket never came up, or dropped mid-stream.
    Network,
    /// The gateway rejected our credentials (bad / expired `/acp` bearer).
    Auth,
    /// The gateway is at capacity (`max_sessions` reached); a retry may land once a
    /// slot frees.
    SlotsFull,
    /// Handshake / framing / JSON-RPC level failure once connected.
    Protocol,
    /// Anything not matched above.
    Other,
}

impl DisconnectReason {
    /// Best-effort classification from an error string — case-insensitive substring
    /// match, most-specific buckets first. Errors here are cosmetic (they pick the
    /// status label), never load-bearing, so an unmatched string just falls to
    /// [`DisconnectReason::Other`].
    pub fn classify(err: &str) -> Self {
        let e = err.to_ascii_lowercase();
        if e.contains("max_sessions")
            || e.contains("too many sessions")
            || e.contains("capacity")
            || e.contains("no free slot")
            || e.contains("503")
            || e.contains("service unavailable")
        {
            Self::SlotsFull
        } else if e.contains("401")
            || e.contains("403")
            || e.contains("unauthorized")
            || e.contains("forbidden")
            || e.contains("invalid token")
        {
            Self::Auth
        } else if e.contains("dns")
            || e.contains("lookup address")
            || e.contains("connect")
            || e.contains("connection reset")
            || e.contains("timed out")
            || e.contains("timeout")
            || e.contains("broken pipe")
            || e.contains("io error")
            || e.contains("os error")
        {
            Self::Network
        } else if e.contains("handshake")
            || e.contains("subprotocol")
            || e.contains("protocol")
            || e.contains("-32")
            || e.contains("unexpected")
        {
            Self::Protocol
        } else {
            Self::Other
        }
    }

    /// Short operator-facing phrase for the status line / log.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Auth => "auth rejected",
            Self::SlotsFull => "server at capacity",
            Self::Protocol => "protocol error",
            Self::Other => "error",
        }
    }
}

/// Backoff before the next reconnect attempt: exponential (1s, 2s, 4s, 8s, 16s)
/// capped at 30s, plus a small deterministic jitter derived from `salt` (e.g. the
/// first byte of the connection id) so a flapping link doesn't retry on an exact
/// cadence. `attempt` is 0-based (0 = first retry after a drop).
pub fn backoff_delay(attempt: u32, salt: u8) -> Duration {
    const CAP_SECS: u64 = 30;
    let secs = (1u64 << attempt.min(5)).min(CAP_SECS); // 1,2,4,8,16,32→cap 30
    let jitter_ms = (salt as u64) * 4 % 1000; // 0..=996 ms, always < 1s
    Duration::from_secs(secs) + Duration::from_millis(jitter_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_maps_known_errors() {
        use DisconnectReason::*;
        // the exact DNS storm string seen in the Activity log
        assert_eq!(
            DisconnectReason::classify(
                "dial wss://…/acp: IO error: failed to lookup address information: nodename nor servname provided"
            ),
            Network
        );
        assert_eq!(
            DisconnectReason::classify("ws read: Connection reset by peer"),
            Network
        );
        assert_eq!(DisconnectReason::classify("HTTP 401 Unauthorized"), Auth);
        assert_eq!(
            DisconnectReason::classify("gateway refused: max_sessions reached"),
            SlotsFull
        );
        assert_eq!(
            DisconnectReason::classify("503 Service Unavailable"),
            SlotsFull
        );
        assert_eq!(
            DisconnectReason::classify("handshake failed: bad subprotocol"),
            Protocol
        );
        assert_eq!(DisconnectReason::classify("something inexplicable"), Other);
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff_delay(0, 0).as_secs(), 1);
        assert_eq!(backoff_delay(1, 0).as_secs(), 2);
        assert_eq!(backoff_delay(2, 0).as_secs(), 4);
        assert_eq!(backoff_delay(3, 0).as_secs(), 8);
        assert_eq!(backoff_delay(4, 0).as_secs(), 16);
        assert_eq!(backoff_delay(5, 0).as_secs(), 30); // 32 → cap
        assert_eq!(backoff_delay(20, 0).as_secs(), 30); // stays capped
    }

    #[test]
    fn backoff_jitter_is_bounded_under_one_second() {
        for salt in [0u8, 1, 42, 127, 249, 255] {
            let extra_ms = backoff_delay(0, salt).as_millis() as u64 - 1000;
            assert!(extra_ms < 1000, "jitter {extra_ms}ms should be < 1s");
        }
    }
}
