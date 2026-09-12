//! Network boundary + pairing authentication (PRODUCT-DESIGN §3.3 / FR-8).
//!
//! Contract:
//! - The server binds `127.0.0.1` by default. Any non-loopback source address
//!   is refused before request processing.
//! - `--public` exposes the server on all interfaces but **requires** pairing
//!   credentials: every connection must present the pairing key (compared
//!   constant-time against a stored hash); wrong or missing keys are refused.
//! - Policy is config/CLI-driven only. It is immutable at the code layer:
//!   natural language can never relax it (former `policy_disabled_by_language`
//!   canary removed; the invariant is structural, not a predicate).

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
    /// Stored pairing secret hash (None = pairing disabled / loopback-only).
    pairing_hash: Option<u64>,
    /// Bind address (loopback unless public).
    bind: IpAddr,
}

/// FNV-1a 64-bit: deterministic, dependency-free secret hashing. The stored
/// value never contains the plaintext key (§7: no secrets at rest).
fn pairing_hash(key: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Constant-time equality of two u64 hashes (branchless OR-accumulate).
fn ct_eq_u64(a: u64, b: u64) -> bool {
    let diff = a ^ b;
    // Fold all bits; `diff == 0` compiles to a data-dependent branch on some
    // targets, this formulation does not.
    (diff | diff.wrapping_neg()).leading_zeros() == 64
}

impl NetworkPolicy {
    /// Resolve the policy. `public && no secret` is a startup error (FR-8:
    /// the server must not come up exposed and unauthenticated).
    pub fn resolve(public: bool, pairing_enabled: bool) -> Result<Self, PolicyError> {
        if public && !pairing_enabled {
            return Err(PolicyError::PublicWithoutPairing);
        }
        let bind: IpAddr = if public {
            "0.0.0.0".parse().expect("valid ip")
        } else {
            DEFAULT_BIND.parse().expect("valid ip")
        };
        Ok(Self {
            public,
            pairing_hash: None,
            bind,
        })
    }

    /// Set the pairing secret from the plaintext key (its hash is stored).
    /// The key source is the `WEBAI_PAIRING_KEY` environment variable,
    /// read by the binary at startup and handed in here.
    pub fn with_pairing_secret(mut self, key: &str) -> Self {
        self.pairing_hash = Some(pairing_hash(key));
        self
    }

    /// Whether the pairing secret is configured.
    pub fn pairing_enabled(&self) -> bool {
        self.pairing_hash.is_some()
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
    /// `presented_pairing` credentials. In public mode the presented key is
    /// hashed and compared constant-time against the stored hash.
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
        // Gate 2: public mode requires a configured pairing secret.
        if self.public && self.pairing_hash.is_none() {
            return Admission::Deny {
                code: "pairing_required",
                reason: "public server requires pairing".into(),
            };
        }
        // Gate 3: public mode validates the presented key against the stored
        // hash (missing and wrong keys are both refused).
        if self.public {
            let Some(stored) = self.pairing_hash else {
                return Admission::Deny {
                    code: "pairing_required",
                    reason: "public server requires pairing".into(),
                };
            };
            let Some(presented) = presented_pairing else {
                return Admission::Deny {
                    code: "pairing_required",
                    reason: "unpaired request refused".into(),
                };
            };
            if !ct_eq_u64(pairing_hash(presented), stored) {
                return Admission::Deny {
                    code: "pairing_invalid",
                    reason: "presented pairing key is not valid".into(),
                };
            }
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

// The former `policy_disabled_by_language` constant-false canary was removed
// (task 87 / review of #69): a always-false predicate proves nothing. The
// invariant it gestured at — policy is config-driven and cannot be relaxed by
// natural language — is structural: no code path reads user text into policy
// decisions.

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
        let policy = NetworkPolicy::resolve(true, true)
            .unwrap()
            .with_pairing_secret("sekrit-key-1");
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
        // Correct key allowed.
        assert!(matches!(
            policy.admit(ip("10.0.0.9"), Some("sekrit-key-1")),
            Admission::Allow
        ));
        // Even loopback needs pairing in public mode.
        let denied_local = policy.admit(ip("127.0.0.1"), None);
        assert!(matches!(denied_local, Admission::Deny { .. }));
    }

    #[test]
    fn wrong_pairing_key_is_refused_constant_time() {
        let policy = NetworkPolicy::resolve(true, true)
            .unwrap()
            .with_pairing_secret("sekrit-key-1");
        // A wrong key is refused with a distinct structured code.
        let denied = policy.admit(ip("10.0.0.9"), Some("wrong-key"));
        assert!(matches!(
            denied,
            Admission::Deny {
                code: "pairing_invalid",
                ..
            }
        ));
        // A key differing by a single byte is also refused.
        let near = policy.admit(ip("10.0.0.9"), Some("sekrit-key-2"));
        assert!(matches!(
            near,
            Admission::Deny {
                code: "pairing_invalid",
                ..
            }
        ));
        // Empty string key: refused.
        let empty = policy.admit(ip("10.0.0.9"), Some(""));
        assert!(matches!(empty, Admission::Deny { .. }));
        // ct_eq_u64 rejects every single-bit hash difference.
        for bit in 0..64u32 {
            assert!(!ct_eq_u64(
                0x0123_4567_89ab_cdef,
                0x0123_4567_89ab_cdef ^ (1 << bit)
            ));
        }
        assert!(ct_eq_u64(42, 42));
    }

    /// Pairing key source: WEBAI_PAIRING_KEY (aligned with the binary's
    /// --public gate). This documents the full production wiring path.
    #[test]
    fn pairing_key_source_documented() {
        // The binary reads WEBAI_PAIRING_KEY and calls with_pairing_secret;
        // this test just locks the storage contract: the plaintext key is
        // never retained on the policy struct.
        let p = NetworkPolicy::resolve(true, true)
            .unwrap()
            .with_pairing_secret("plain-visible-key");
        let debug = format!("{p:?}");
        assert!(
            !debug.contains("plain-visible-key"),
            "no plaintext in Debug"
        );
    }
}
