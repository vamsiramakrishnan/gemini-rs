//! Full credential type hierarchy matching ADK-JS `AuthCredential`.

use serde::{Deserialize, Serialize};

/// The type of authentication credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuthCredentialType {
    ApiKey,
    Http,
    #[serde(rename = "OAUTH2")]
    OAuth2,
    OpenIdConnect,
    ServiceAccount,
}

/// HTTP credentials — basic auth or bearer token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpCredentials {
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
}

/// HTTP authentication with a named scheme (e.g. "bearer", "basic").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpAuth {
    pub scheme: String,
    pub credentials: HttpCredentials,
}

/// OAuth2 authentication data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2Auth {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub auth_uri: Option<String>,
    pub token_uri: Option<String>,
    pub redirect_uri: Option<String>,
    pub auth_code: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub scopes: Option<Vec<String>>,
    pub auth_response_uri: Option<String>,
}

/// Service account credential for Google Cloud.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAccountCredential {
    pub service_account_key_file: Option<String>,
    pub service_account_key: Option<serde_json::Value>,
    pub scopes: Option<Vec<String>>,
    pub use_default_credential: Option<bool>,
    pub project_id: Option<String>,
    pub universe_domain: Option<String>,
}

/// Full auth credential — discriminated by `auth_type` with variant-specific data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthCredential {
    pub auth_type: AuthCredentialType,
    pub resource_ref: Option<String>,
    // Variant-specific auth data
    pub api_key: Option<String>,
    pub http: Option<HttpAuth>,
    pub oauth2: Option<OAuth2Auth>,
    pub service_account: Option<ServiceAccountCredential>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_type_screaming_snake_case() {
        assert_eq!(
            serde_json::to_string(&AuthCredentialType::ApiKey).unwrap(),
            "\"API_KEY\""
        );
        assert_eq!(
            serde_json::to_string(&AuthCredentialType::Http).unwrap(),
            "\"HTTP\""
        );
        assert_eq!(
            serde_json::to_string(&AuthCredentialType::OAuth2).unwrap(),
            "\"OAUTH2\""
        );
        assert_eq!(
            serde_json::to_string(&AuthCredentialType::OpenIdConnect).unwrap(),
            "\"OPEN_ID_CONNECT\""
        );
        assert_eq!(
            serde_json::to_string(&AuthCredentialType::ServiceAccount).unwrap(),
            "\"SERVICE_ACCOUNT\""
        );
    }

    #[test]
    fn credential_type_roundtrip() {
        let types = [
            AuthCredentialType::ApiKey,
            AuthCredentialType::Http,
            AuthCredentialType::OAuth2,
            AuthCredentialType::OpenIdConnect,
            AuthCredentialType::ServiceAccount,
        ];
        for t in &types {
            let json = serde_json::to_string(t).unwrap();
            let parsed: AuthCredentialType = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, t);
        }
    }

    #[test]
    fn api_key_credential_roundtrip() {
        let cred = AuthCredential {
            auth_type: AuthCredentialType::ApiKey,
            resource_ref: Some("my-resource".into()),
            api_key: Some("sk-secret-123".into()),
            http: None,
            oauth2: None,
            service_account: None,
        };

        let json = serde_json::to_string_pretty(&cred).unwrap();
        let parsed: AuthCredential = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.auth_type, AuthCredentialType::ApiKey);
        assert_eq!(parsed.api_key.as_deref(), Some("sk-secret-123"));
        assert_eq!(parsed.resource_ref.as_deref(), Some("my-resource"));
        assert!(parsed.http.is_none());
        assert!(parsed.oauth2.is_none());
        assert!(parsed.service_account.is_none());
    }

    #[test]
    fn http_credential_roundtrip() {
        let cred = AuthCredential {
            auth_type: AuthCredentialType::Http,
            resource_ref: None,
            api_key: None,
            http: Some(HttpAuth {
                scheme: "bearer".into(),
                credentials: HttpCredentials {
                    username: None,
                    password: None,
                    token: Some("eyJhbGciOi...".into()),
                },
            }),
            oauth2: None,
            service_account: None,
        };

        let json = serde_json::to_string(&cred).unwrap();
        let parsed: AuthCredential = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.auth_type, AuthCredentialType::Http);
        let http = parsed.http.unwrap();
        assert_eq!(http.scheme, "bearer");
        assert_eq!(http.credentials.token.as_deref(), Some("eyJhbGciOi..."));
    }

    #[test]
    fn oauth2_credential_roundtrip() {
        let cred = AuthCredential {
            auth_type: AuthCredentialType::OAuth2,
            resource_ref: None,
            api_key: None,
            http: None,
            oauth2: Some(OAuth2Auth {
                client_id: Some("client-123".into()),
                client_secret: Some("secret-456".into()),
                auth_uri: Some("https://accounts.google.com/o/oauth2/auth".into()),
                token_uri: Some("https://oauth2.googleapis.com/token".into()),
                redirect_uri: Some("http://localhost:8080/callback".into()),
                auth_code: None,
                access_token: Some("ya29.access".into()),
                refresh_token: Some("1//refresh".into()),
                expires_at: Some(1700000000),
                scopes: Some(vec!["openid".into(), "email".into()]),
                auth_response_uri: None,
            }),
            service_account: None,
        };

        let json = serde_json::to_string(&cred).unwrap();
        let parsed: AuthCredential = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.auth_type, AuthCredentialType::OAuth2);
        let oauth2 = parsed.oauth2.unwrap();
        assert_eq!(oauth2.client_id.as_deref(), Some("client-123"));
        assert_eq!(oauth2.scopes.as_ref().unwrap().len(), 2);
        assert_eq!(oauth2.expires_at, Some(1700000000));
    }

    #[test]
    fn service_account_credential_roundtrip() {
        let cred = AuthCredential {
            auth_type: AuthCredentialType::ServiceAccount,
            resource_ref: None,
            api_key: None,
            http: None,
            oauth2: None,
            service_account: Some(ServiceAccountCredential {
                service_account_key_file: Some("/path/to/key.json".into()),
                service_account_key: None,
                scopes: Some(vec!["https://www.googleapis.com/auth/cloud-platform".into()]),
                use_default_credential: Some(true),
                project_id: Some("my-project".into()),
                universe_domain: Some("googleapis.com".into()),
            }),
        };

        let json = serde_json::to_string(&cred).unwrap();
        let parsed: AuthCredential = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.auth_type, AuthCredentialType::ServiceAccount);
        let sa = parsed.service_account.unwrap();
        assert_eq!(sa.service_account_key_file.as_deref(), Some("/path/to/key.json"));
        assert_eq!(sa.use_default_credential, Some(true));
        assert_eq!(sa.project_id.as_deref(), Some("my-project"));
    }
}
