//! Credential service — secure storage and retrieval of auth credentials.

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// An authentication credential with optional token, refresh, and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthCredential {
    /// The type of credential (e.g., "oauth2", "api_key", "service_account").
    pub credential_type: String,
    /// The primary token (access token, API key, etc.).
    pub token: Option<String>,
    /// Refresh token for obtaining new access tokens.
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) when the credential expires.
    pub expires_at: Option<u64>,
    /// Additional metadata (e.g., scopes, provider-specific fields).
    pub metadata: serde_json::Value,
}

/// Errors from credential service operations.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The requested credential was not found.
    #[error("Credential not found")]
    NotFound,
    /// A storage backend error.
    #[error("{0}")]
    Storage(String),
}

/// Trait for credential persistence — load, save, delete.
#[async_trait]
pub trait CredentialService: Send + Sync {
    /// Load a credential by key. Returns `None` if not found.
    async fn load_credential(&self, key: &str) -> Result<Option<AuthCredential>, CredentialError>;

    /// Save a credential under the given key.
    async fn save_credential(
        &self,
        key: &str,
        credential: AuthCredential,
    ) -> Result<(), CredentialError>;

    /// Delete a credential by key.
    async fn delete_credential(&self, key: &str) -> Result<(), CredentialError>;
}

/// In-memory credential service for testing and development.
pub struct InMemoryCredentialService {
    inner: DashMap<String, AuthCredential>,
}

impl InMemoryCredentialService {
    /// Create a new empty in-memory credential service.
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }
}

impl Default for InMemoryCredentialService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CredentialService for InMemoryCredentialService {
    async fn load_credential(&self, key: &str) -> Result<Option<AuthCredential>, CredentialError> {
        Ok(self.inner.get(key).map(|entry| entry.value().clone()))
    }

    async fn save_credential(
        &self,
        key: &str,
        credential: AuthCredential,
    ) -> Result<(), CredentialError> {
        self.inner.insert(key.to_string(), credential);
        Ok(())
    }

    async fn delete_credential(&self, key: &str) -> Result<(), CredentialError> {
        self.inner.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_credential() -> AuthCredential {
        AuthCredential {
            credential_type: "oauth2".to_string(),
            token: Some("access-token-123".to_string()),
            refresh_token: Some("refresh-456".to_string()),
            expires_at: Some(1700000000),
            metadata: serde_json::json!({"scope": "read write"}),
        }
    }

    #[tokio::test]
    async fn save_and_load() {
        let svc = InMemoryCredentialService::new();
        let cred = sample_credential();
        svc.save_credential("my-key", cred.clone()).await.unwrap();

        let loaded = svc.load_credential("my-key").await.unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.credential_type, "oauth2");
        assert_eq!(loaded.token, Some("access-token-123".to_string()));
    }

    #[tokio::test]
    async fn load_nonexistent_returns_none() {
        let svc = InMemoryCredentialService::new();
        let loaded = svc.load_credential("missing").await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn delete_credential() {
        let svc = InMemoryCredentialService::new();
        svc.save_credential("key", sample_credential())
            .await
            .unwrap();
        svc.delete_credential("key").await.unwrap();

        let loaded = svc.load_credential("key").await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn overwrite_credential() {
        let svc = InMemoryCredentialService::new();
        svc.save_credential("key", sample_credential())
            .await
            .unwrap();

        let updated = AuthCredential {
            credential_type: "api_key".to_string(),
            token: Some("new-token".to_string()),
            refresh_token: None,
            expires_at: None,
            metadata: serde_json::json!({}),
        };
        svc.save_credential("key", updated).await.unwrap();

        let loaded = svc.load_credential("key").await.unwrap().unwrap();
        assert_eq!(loaded.credential_type, "api_key");
        assert_eq!(loaded.token, Some("new-token".to_string()));
    }

    #[test]
    fn credential_service_is_object_safe() {
        fn _assert(_: &dyn CredentialService) {}
    }

    #[test]
    fn auth_credential_serde_roundtrip() {
        let cred = sample_credential();
        let json = serde_json::to_string(&cred).unwrap();
        let parsed: AuthCredential = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.credential_type, "oauth2");
        assert_eq!(parsed.token, Some("access-token-123".to_string()));
    }
}
