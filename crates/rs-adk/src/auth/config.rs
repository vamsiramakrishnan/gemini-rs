//! Auth configuration and tool argument types.

use serde::{Deserialize, Serialize};

use super::credential::AuthCredential;
use super::schemes::AuthScheme;

/// Configuration binding a security scheme to credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// The security scheme that describes how authentication is performed.
    pub auth_scheme: AuthScheme,
    /// The raw (user-provided) credential before any exchange.
    pub raw_auth_credential: Option<AuthCredential>,
    /// The credential after an exchange (e.g. code-for-token swap).
    pub exchanged_auth_credential: Option<AuthCredential>,
    /// Key used to look up/store the credential in a credential service.
    pub credential_key: String,
}

/// Arguments passed to a tool when authentication is required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthToolArguments {
    /// The function call ID this auth request is associated with.
    pub function_call_id: String,
    /// The auth configuration describing what credential is needed.
    pub auth_config: AuthConfig,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::credential::{AuthCredentialType, OAuth2Auth};
    use crate::auth::schemes::OAuthGrantType;

    #[test]
    fn auth_config_roundtrip() {
        let config = AuthConfig {
            auth_scheme: AuthScheme::OAuth2 {
                grant_type: Some(OAuthGrantType::AuthorizationCode),
                authorization_url: Some("https://example.com/authorize".into()),
                token_url: Some("https://example.com/token".into()),
                scopes: None,
            },
            raw_auth_credential: Some(AuthCredential {
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
                    scopes: Some(vec!["openid".into()]),
                    auth_response_uri: None,
                }),
                service_account: None,
            }),
            exchanged_auth_credential: None,
            credential_key: "my-oauth-cred".into(),
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: AuthConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.credential_key, "my-oauth-cred");
        assert!(parsed.raw_auth_credential.is_some());
        assert!(parsed.exchanged_auth_credential.is_none());

        let raw = parsed.raw_auth_credential.unwrap();
        assert_eq!(raw.auth_type, AuthCredentialType::OAuth2);
        let oauth2 = raw.oauth2.unwrap();
        assert_eq!(oauth2.client_id.as_deref(), Some("client-123"));
        assert_eq!(oauth2.auth_code.as_deref(), Some("auth-code-789"));
    }

    #[test]
    fn auth_tool_arguments_roundtrip() {
        let args = AuthToolArguments {
            function_call_id: "fc-001".into(),
            auth_config: AuthConfig {
                auth_scheme: AuthScheme::ApiKey {
                    location: "header".into(),
                    name: "X-API-Key".into(),
                },
                raw_auth_credential: Some(AuthCredential {
                    auth_type: AuthCredentialType::ApiKey,
                    resource_ref: None,
                    api_key: Some("my-api-key".into()),
                    http: None,
                    oauth2: None,
                    service_account: None,
                }),
                exchanged_auth_credential: None,
                credential_key: "api-key-cred".into(),
            },
        };

        let json = serde_json::to_string(&args).unwrap();
        let parsed: AuthToolArguments = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.function_call_id, "fc-001");
        assert_eq!(parsed.auth_config.credential_key, "api-key-cred");
        let raw = parsed.auth_config.raw_auth_credential.unwrap();
        assert_eq!(raw.api_key.as_deref(), Some("my-api-key"));
    }

    #[test]
    fn auth_config_with_exchanged_credential() {
        let config = AuthConfig {
            auth_scheme: AuthScheme::Http {
                scheme: "bearer".into(),
                bearer_format: Some("JWT".into()),
            },
            raw_auth_credential: None,
            exchanged_auth_credential: Some(AuthCredential {
                auth_type: AuthCredentialType::OAuth2,
                resource_ref: None,
                api_key: None,
                http: None,
                oauth2: Some(OAuth2Auth {
                    client_id: None,
                    client_secret: None,
                    auth_uri: None,
                    token_uri: None,
                    redirect_uri: None,
                    auth_code: None,
                    access_token: Some("ya29.exchanged-token".into()),
                    refresh_token: Some("1//refresh".into()),
                    expires_at: Some(1700000000),
                    scopes: None,
                    auth_response_uri: None,
                }),
                service_account: None,
            }),
            credential_key: "exchanged-cred".into(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: AuthConfig = serde_json::from_str(&json).unwrap();

        assert!(parsed.raw_auth_credential.is_none());
        let exchanged = parsed.exchanged_auth_credential.unwrap();
        assert_eq!(
            exchanged.oauth2.as_ref().unwrap().access_token.as_deref(),
            Some("ya29.exchanged-token")
        );
    }
}
