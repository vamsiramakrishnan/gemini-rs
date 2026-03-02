//! Credential exchanger — trait and registry for exchanging/transforming credentials
//! (e.g. auth code to access token).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::credential::AuthCredential;
use super::schemes::AuthScheme;

/// Error from credential exchange.
#[derive(Debug, thiserror::Error)]
pub enum CredentialExchangeError {
    #[error("Exchange failed: {0}")]
    ExchangeFailed(String),
    #[error("No exchanger registered for scheme type: {0}")]
    NoExchanger(String),
    #[error("{0}")]
    Other(String),
}

/// Trait for exchanging/transforming credentials (e.g. auth code -> access token).
#[async_trait]
pub trait CredentialExchanger: Send + Sync {
    async fn exchange(
        &self,
        credential: &AuthCredential,
        scheme: Option<&AuthScheme>,
    ) -> Result<AuthCredential, CredentialExchangeError>;
}

/// Registry of credential exchangers, keyed by scheme type name.
pub struct CredentialExchangerRegistry {
    exchangers: HashMap<String, Arc<dyn CredentialExchanger>>,
}

impl CredentialExchangerRegistry {
    pub fn new() -> Self {
        Self {
            exchangers: HashMap::new(),
        }
    }

    pub fn register(&mut self, scheme_type: &str, exchanger: Arc<dyn CredentialExchanger>) {
        self.exchangers.insert(scheme_type.to_string(), exchanger);
    }

    pub async fn exchange(
        &self,
        credential: &AuthCredential,
        scheme: &AuthScheme,
    ) -> Result<AuthCredential, CredentialExchangeError> {
        let scheme_type = match scheme {
            AuthScheme::ApiKey { .. } => "apiKey",
            AuthScheme::Http { .. } => "http",
            AuthScheme::OAuth2 { .. } => "oauth2",
            AuthScheme::OpenIdConnect { .. } => "openIdConnect",
        };
        let exchanger = self
            .exchangers
            .get(scheme_type)
            .ok_or_else(|| CredentialExchangeError::NoExchanger(scheme_type.to_string()))?;
        exchanger.exchange(credential, Some(scheme)).await
    }
}

impl Default for CredentialExchangerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{AuthCredentialType, OAuth2Auth};
    use crate::auth::schemes::OAuthGrantType;

    /// Mock exchanger that just sets the access_token to "exchanged".
    struct MockExchanger;

    #[async_trait]
    impl CredentialExchanger for MockExchanger {
        async fn exchange(
            &self,
            credential: &AuthCredential,
            _scheme: Option<&AuthScheme>,
        ) -> Result<AuthCredential, CredentialExchangeError> {
            let mut result = credential.clone();
            if let Some(ref mut oauth2) = result.oauth2 {
                oauth2.access_token = Some("exchanged".into());
            }
            Ok(result)
        }
    }

    fn test_credential() -> AuthCredential {
        AuthCredential {
            auth_type: AuthCredentialType::OAuth2,
            resource_ref: None,
            api_key: None,
            http: None,
            oauth2: Some(OAuth2Auth {
                client_id: Some("client-123".into()),
                client_secret: Some("secret-456".into()),
                auth_uri: None,
                token_uri: None,
                redirect_uri: None,
                auth_code: Some("auth-code-789".into()),
                access_token: None,
                refresh_token: None,
                expires_at: None,
                scopes: None,
                auth_response_uri: None,
            }),
            service_account: None,
        }
    }

    #[tokio::test]
    async fn register_and_exchange_with_mock() {
        let mut registry = CredentialExchangerRegistry::new();
        registry.register("oauth2", Arc::new(MockExchanger));

        let cred = test_credential();
        let scheme = AuthScheme::OAuth2 {
            grant_type: Some(OAuthGrantType::AuthorizationCode),
            authorization_url: Some("https://example.com/authorize".into()),
            token_url: Some("https://example.com/token".into()),
            scopes: None,
        };

        let result = registry.exchange(&cred, &scheme).await.unwrap();
        assert_eq!(
            result.oauth2.as_ref().unwrap().access_token.as_deref(),
            Some("exchanged")
        );
        // Original fields preserved
        assert_eq!(
            result.oauth2.as_ref().unwrap().client_id.as_deref(),
            Some("client-123")
        );
    }

    #[tokio::test]
    async fn exchange_unregistered_scheme_returns_error() {
        let registry = CredentialExchangerRegistry::new();

        let cred = test_credential();
        let scheme = AuthScheme::ApiKey {
            location: "header".into(),
            name: "X-API-Key".into(),
        };

        let result = registry.exchange(&cred, &scheme).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            CredentialExchangeError::NoExchanger(scheme_type) => {
                assert_eq!(scheme_type, "apiKey");
            }
            _ => panic!("expected NoExchanger error"),
        }
    }

    #[test]
    fn credential_exchanger_trait_is_object_safe() {
        // This test verifies that the trait can be used as a trait object.
        // If CredentialExchanger is not object-safe, this will fail to compile.
        fn _assert_object_safe(_: &dyn CredentialExchanger) {}
        fn _assert_arc_object_safe(_: Arc<dyn CredentialExchanger>) {}
    }

    #[test]
    fn default_registry_is_empty() {
        let registry = CredentialExchangerRegistry::default();
        assert!(registry.exchangers.is_empty());
    }
}
