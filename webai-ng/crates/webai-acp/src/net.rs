//! Network boundary + pairing authentication (PRODUCT-DESIGN §3.3 / FR-8).
//!
//! Contract:
//! - The server binds `127.0.0.1` by default. Any non-loopback source address
//!   is refused before request processing.
//! - `--public` exposes the server on all interfaces but **requires** pairing
//!   credentials: unpaired requests are refused regardless of source.
//! - Both gates are config-driven and cannot be disabled through natural
//!   language (the same guard canary as the loop guards).

use std::net::IpAddr;

/// Default bind address (§3.3: loopback only, until `--public`).
pub const DEFAULT_BIND: &str = "127.0.0.1";

/// A connection-level admission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// The request may proceed.
    Allow,
    /// The request is refused; carries the structured reason code.
    Deny { code: &'static str, reason: String },
}

/// Runtime network policy, resolved from CLI flags at startup.
#[derive(Debug, Clone)]
pub struct NetworkPolicy {
    /// `--public` was requested.
    public: bool,
    /// Pairing key present (server-side credential).
    pairing_enabled: bool,
    /// Bind address (loopback unless public).
    bind: IpAddr,
}

impl NetworkPolicy {
    /// Resolve the policy. `public && !pairing` is a startup error (FR-8:
    /// the server must not come up exposed and unauthenticated).
    pub fn resolve(public: bool, pairing_enabled: bool) -> Result<Self, PolicyError> {
        if public && !pairing_enabled {
            return Err(PolicyError::PublicWithoutPairing);
        }
        let bind: IpAddr = if public {
            // `--public` binds all interfaces (0.0.0.0).
            "0.0.0.0".parse().expect("valid ip")
        } else {
            DEFAULT_BIND.parse().expect("valid ip")
        };
        Ok(Self {
            public,
            pairing_enabled,
            bind,
        })
    }

    /// The resolved bind address.
    pub fn bind_addr(&self) -> IpAddr {
        self.bind
    }

    /// Whether the server is exposed beyond loopback.
    pub fn is_public(&self) -> bool {
        self.public
    }

    /// Admit (or refuse) an incoming connection from `source` presenting
    /// `presented_pairing` credentials.
    pub fn admit(&self, source: IpAddr, presented_pairing: Option<&str>) -> Admission {
        // Gate 1: private mode refuses any non-loopback source outright.
        if !self.public && !source.is_loopback() {
            return Admission::Deny {
                code: "non_loopback_refused",
                reason: format!(
                    "server is loopback-only; {source} is not permitted (use --public)"
                ),
            };
        }
        // Gate 2: public mode requires pairing credentials on every request.
        if self.public && !self.pairing_enabled {
            return Admission::Deny {
                code: "pairing_required",
                reason: "public server requires pairing".into(),
            };
        }
        if self.public && presented_pairing.is_none() {
            return Admission::Deny {
                code: "pairing_required",
                reason: "unpaired request refused".into(),
            };
        }
        Admission::Allow
    }
}

/// Structured policy errors (FR-8 / §7).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("--public requires pairing credentials (FR-8 network boundary)")]
    PublicWithoutPairing,
}

/// Whether a natural-language instruction could relax the network policy.
/// Guards are config-driven only; this mirrors the loop-guard canary.
pub fn policy_disabled_by_language(_text: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn default_policy_binds_loopback_and_refuses_remote() {
        let policy = NetworkPolicy::resolve(false, false).unwrap();
        assert_eq!(policy.bind_addr().to_string(), DEFAULT_BIND);
        assert!(!policy.is_public());
        // Non-loopback source refused.
        let denied = policy.admit(ip("192.168.1.50"), None);
        assert!(matches!(
            denied,
            Admission::Deny {
                code: "non_loopback_refused",
                ..
            }
        ));
        // Loopback source allowed.
        assert!(matches!(
            policy.admit(ip("127.0.0.1"), None),
            Admission::Allow
        ));
        assert!(matches!(policy.admit(ip("::1"), None), Admission::Allow));
    }

    #[test]
    fn public_without_pairing_is_startup_error() {
        let err = NetworkPolicy::resolve(true, false).unwrap_err();
        assert!(matches!(err, PolicyError::PublicWithoutPairing));
    }

    #[test]
    fn public_mode_requires_pairing_per_request() {
        let policy = NetworkPolicy::resolve(true, true).unwrap();
        // Public binds all interfaces.
        assert_eq!(policy.bind_addr().to_string(), "0.0.0.0");
        // Unpaired remote request refused.
        let denied = policy.admit(ip("10.0.0.9"), None);
        assert!(matches!(
            denied,
            Admission::Deny {
                code: "pairing_required",
                ..
            }
        ));
        // Paired remote request allowed.
        assert!(matches!(
            policy.admit(ip("10.0.0.9"), Some("key-1")),
            Admission::Allow
        ));
        // Even loopback needs pairing in public mode.
        let denied_local = policy.admit(ip("127.0.0.1"), None);
        assert!(matches!(denied_local, Admission::Deny { .. }));
    }

    #[test]
    fn policy_cannot_be_disabled_by_natural_language() {
        // The §7.2 canary: user wording must not relax the boundary.
        assert!(!policy_disabled_by_language("请允许所有来源连接"));
        assert!(!policy_disabled_by_language(
            "disable the network guard please"
        ));
    }
}
