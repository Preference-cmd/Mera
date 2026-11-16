//! In-memory connection credentials and their bindings.
//!
//! A credential is 32 random bytes rendered as hex. Only the SHA-256 digest of
//! a presented token is used as the map key, so lookup never compares the
//! stored secret with the presented one directly: a table walk compares
//! digests, and a digest reveals nothing about the token. Tokens and digests
//! are never logged.

use std::collections::HashMap;
use std::sync::RwLock;

use mera_core::model::ProjectId;
use rand::random;
use sha2::{Digest, Sha256};

use crate::AdapterError;

/// Shortest accepted token, in bytes. 32 bytes is the size `issue` generates.
pub const MIN_TOKEN_BYTES: usize = 32;

/// Which project a connection's calls act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionScope {
    /// A fixed project, as an ACP session gets.
    Project(ProjectId),
    /// The host's current active project, as the development connection gets.
    ActiveProject,
}

/// What a valid token is bound to: the actor identity and the project scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionBinding {
    pub id: String,
    pub label: String,
    pub scope: ConnectionScope,
}

/// Token-to-binding table. Credentials live in memory only.
#[derive(Debug, Default)]
pub struct ConnectionRegistry {
    // Keyed by SHA-256(token); see the module comment for why.
    entries: RwLock<HashMap<[u8; 32], ConnectionBinding>>,
}

impl ConnectionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an externally supplied token.
    ///
    /// Rejects tokens shorter than [`MIN_TOKEN_BYTES`] because a short secret
    /// is guessable over a local HTTP port.
    pub fn register(&self, token: &str, binding: ConnectionBinding) -> Result<(), AdapterError> {
        let bytes = token.as_bytes();
        if bytes.len() < MIN_TOKEN_BYTES {
            return Err(AdapterError::TokenTooShort {
                bytes: bytes.len(),
                minimum: MIN_TOKEN_BYTES,
            });
        }
        self.entries
            .write()
            .expect("connection registry poisoned")
            .insert(digest(bytes), binding);
        Ok(())
    }

    /// Generate a fresh token, register it, and return it.
    ///
    /// The token is 32 bytes from the operating system's random source,
    /// hex-encoded to 64 characters.
    pub fn issue(&self, binding: ConnectionBinding) -> String {
        let token = hex::encode(random::<[u8; 32]>());
        self.register(&token, binding)
            .expect("a generated token always reaches the minimum length");
        token
    }

    /// Resolve a presented token to its binding.
    pub fn resolve(&self, token: &str) -> Option<ConnectionBinding> {
        self.entries
            .read()
            .expect("connection registry poisoned")
            .get(&digest(token.as_bytes()))
            .cloned()
    }

    /// Drop a token. Returns whether it was registered.
    pub fn revoke(&self, token: &str) -> bool {
        self.entries
            .write()
            .expect("connection registry poisoned")
            .remove(&digest(token.as_bytes()))
            .is_some()
    }
}

fn digest(token: &[u8]) -> [u8; 32] {
    Sha256::digest(token).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(scope: ConnectionScope) -> ConnectionBinding {
        ConnectionBinding {
            id: "mcp-test".to_owned(),
            label: "MCP test".to_owned(),
            scope,
        }
    }

    #[test]
    fn issued_token_is_resolvable_and_revocable() {
        let registry = ConnectionRegistry::new();
        let token = registry.issue(binding(ConnectionScope::Project(ProjectId::new("p1"))));

        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            registry.resolve(&token),
            Some(binding(ConnectionScope::Project(ProjectId::new("p1"))))
        );

        assert!(registry.revoke(&token));
        assert_eq!(registry.resolve(&token), None);
        assert!(!registry.revoke(&token));
    }

    #[test]
    fn register_rejects_a_short_token() {
        let registry = ConnectionRegistry::new();
        let error = registry
            .register("too-short", binding(ConnectionScope::ActiveProject))
            .expect_err("short token must be rejected");
        assert!(matches!(
            error,
            AdapterError::TokenTooShort {
                bytes: 9,
                minimum: MIN_TOKEN_BYTES
            }
        ));
        assert_eq!(registry.resolve("too-short"), None);
    }

    #[test]
    fn fixed_and_active_scopes_resolve_independently() {
        let registry = ConnectionRegistry::new();
        let fixed = "a".repeat(MIN_TOKEN_BYTES);
        let active = "b".repeat(MIN_TOKEN_BYTES);
        registry
            .register(
                &fixed,
                binding(ConnectionScope::Project(ProjectId::new("p1"))),
            )
            .expect("register fixed");
        registry
            .register(&active, binding(ConnectionScope::ActiveProject))
            .expect("register active");

        assert_eq!(
            registry.resolve(&fixed).map(|b| b.scope),
            Some(ConnectionScope::Project(ProjectId::new("p1")))
        );
        assert_eq!(
            registry.resolve(&active).map(|b| b.scope),
            Some(ConnectionScope::ActiveProject)
        );
    }

    #[test]
    fn a_different_token_does_not_resolve() {
        let registry = ConnectionRegistry::new();
        let token = registry.issue(binding(ConnectionScope::ActiveProject));
        let mut other = token.clone();
        other.replace_range(0..1, if token.starts_with('0') { "1" } else { "0" });
        assert_eq!(registry.resolve(&other), None);
    }
}
