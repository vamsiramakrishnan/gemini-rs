//! Auth handler — orchestrates auth flows by retrieving stored credentials
//! and generating auth requests.

use super::config::AuthConfig;
use super::credential::AuthCredential;
use crate::state::State;

/// Orchestrates auth flows: retrieves stored credentials and generates auth requests.
pub struct AuthHandler {
    config: AuthConfig,
}

impl AuthHandler {
    /// Create a new auth handler with the given configuration.
    pub fn new(config: AuthConfig) -> Self {
        Self { config }
    }

    /// Retrieve the auth credential from state (looks up "temp:{credential_key}").
    pub fn get_auth_response(&self, state: &State) -> Option<AuthCredential> {
        let key = format!("temp:{}", self.config.credential_key);
        state
            .get_raw(&key)
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// Generate an auth request config for the client to fulfill (e.g. OAuth2 redirect).
    pub fn generate_auth_request(&self) -> AuthConfig {
        self.config.clone()
    }

    /// Get the auth config.
    pub fn config(&self) -> &AuthConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{AuthCredentialType, OAuth2Auth};
    use crate::auth::schemes::{AuthScheme, OAuthGrantType};

    fn test_config() -> AuthConfig {
        AuthConfig {
            auth_scheme: AuthScheme::OAuth2 {
                grant_type: Some(OAuthGrantType::AuthorizationCode),
                authorization_url: Some("https://example.com/authorize".into()),
                token_url: Some("https://example.com/token".into()),
                scopes: None,
            },
            raw_auth_credential: None,
            exchanged_auth_credential: None,
            credential_key: "my-oauth-cred".into(),
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
                client_secret: None,
                auth_uri: None,
                token_uri: None,
                redirect_uri: None,
                auth_code: None,
                access_token: Some("ya29.test-token".into()),
                refresh_token: None,
                expires_at: None,
                scopes: None,
                auth_response_uri: None,
            }),
            service_account: None,
        }
    }

    #[test]
    fn get_auth_response_found() {
        let state = State::new();
        let cred = test_credential();
        state.set("temp:my-oauth-cred", &cred);

        let handler = AuthHandler::new(test_config());
        let result = handler.get_auth_response(&state);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.auth_type, AuthCredentialType::OAuth2);
        assert_eq!(
            result.oauth2.as_ref().unwrap().access_token.as_deref(),
            Some("ya29.test-token")
        );
    }

    #[test]
    fn get_auth_response_not_found() {
        let state = State::new();
        let handler = AuthHandler::new(test_config());
        let result = handler.get_auth_response(&state);
        assert!(result.is_none());
    }

    #[test]
    fn generate_auth_request_returns_config_clone() {
        let config = test_config();
        let handler = AuthHandler::new(config.clone());
        let request = handler.generate_auth_request();

        assert_eq!(request.credential_key, config.credential_key);
        // Verify it's a separate clone by checking the values match
        match (&request.auth_scheme, &config.auth_scheme) {
            (
                AuthScheme::OAuth2 {
                    authorization_url: a_url,
                    ..
                },
                AuthScheme::OAuth2 {
                    authorization_url: b_url,
                    ..
                },
            ) => {
                assert_eq!(a_url, b_url);
            }
            _ => panic!("expected OAuth2 scheme"),
        }
    }

    #[test]
    fn config_accessor() {
        let config = test_config();
        let handler = AuthHandler::new(config);
        assert_eq!(handler.config().credential_key, "my-oauth-cred");
    }
}
