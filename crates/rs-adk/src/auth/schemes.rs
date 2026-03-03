//! OpenAPI 3.0-style security scheme definitions.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// OAuth2 grant type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthGrantType {
    /// Client credentials grant (machine-to-machine).
    ClientCredentials,
    /// Authorization code grant (user login flow).
    AuthorizationCode,
    /// Implicit grant (legacy browser flow).
    Implicit,
    /// Resource owner password grant.
    Password,
}

/// OpenAPI 3.0-style security scheme — internally tagged on `"type"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AuthScheme {
    /// API key passed via header, query, or cookie.
    #[serde(rename = "apiKey")]
    ApiKey {
        /// Where the API key is sent: "header", "query", or "cookie".
        #[serde(rename = "in")]
        location: String,
        /// The header/param/cookie name.
        name: String,
    },
    /// HTTP authentication (bearer, basic, etc.).
    #[serde(rename = "http")]
    Http {
        /// The HTTP auth scheme (e.g. "bearer", "basic").
        scheme: String,
        /// Optional format hint for bearer tokens.
        #[serde(skip_serializing_if = "Option::is_none")]
        bearer_format: Option<String>,
    },
    /// OAuth2 authentication.
    #[serde(rename = "oauth2")]
    OAuth2 {
        /// The OAuth2 grant type.
        #[serde(skip_serializing_if = "Option::is_none")]
        grant_type: Option<OAuthGrantType>,
        /// Authorization endpoint URL.
        #[serde(skip_serializing_if = "Option::is_none")]
        authorization_url: Option<String>,
        /// Token endpoint URL.
        #[serde(skip_serializing_if = "Option::is_none")]
        token_url: Option<String>,
        /// Available scopes (name to description mapping).
        #[serde(skip_serializing_if = "Option::is_none")]
        scopes: Option<HashMap<String, String>>,
    },
    /// OpenID Connect Discovery.
    #[serde(rename = "openIdConnect")]
    OpenIdConnect {
        /// URL of the OpenID Connect discovery document.
        open_id_connect_url: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_grant_type_snake_case() {
        assert_eq!(
            serde_json::to_string(&OAuthGrantType::ClientCredentials).unwrap(),
            "\"client_credentials\""
        );
        assert_eq!(
            serde_json::to_string(&OAuthGrantType::AuthorizationCode).unwrap(),
            "\"authorization_code\""
        );
        assert_eq!(
            serde_json::to_string(&OAuthGrantType::Implicit).unwrap(),
            "\"implicit\""
        );
        assert_eq!(
            serde_json::to_string(&OAuthGrantType::Password).unwrap(),
            "\"password\""
        );
    }

    #[test]
    fn oauth_grant_type_roundtrip() {
        let types = [
            OAuthGrantType::ClientCredentials,
            OAuthGrantType::AuthorizationCode,
            OAuthGrantType::Implicit,
            OAuthGrantType::Password,
        ];
        for t in &types {
            let json = serde_json::to_string(t).unwrap();
            let parsed: OAuthGrantType = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, t);
        }
    }

    #[test]
    fn api_key_scheme_tagged_serialization() {
        let scheme = AuthScheme::ApiKey {
            location: "header".into(),
            name: "X-API-Key".into(),
        };

        let json = serde_json::to_value(&scheme).unwrap();
        assert_eq!(json["type"], "apiKey");
        assert_eq!(json["in"], "header");
        assert_eq!(json["name"], "X-API-Key");

        // Roundtrip
        let parsed: AuthScheme = serde_json::from_value(json).unwrap();
        match parsed {
            AuthScheme::ApiKey { location, name } => {
                assert_eq!(location, "header");
                assert_eq!(name, "X-API-Key");
            }
            _ => panic!("expected ApiKey variant"),
        }
    }

    #[test]
    fn http_scheme_tagged_serialization() {
        let scheme = AuthScheme::Http {
            scheme: "bearer".into(),
            bearer_format: Some("JWT".into()),
        };

        let json = serde_json::to_value(&scheme).unwrap();
        assert_eq!(json["type"], "http");
        assert_eq!(json["scheme"], "bearer");
        assert_eq!(json["bearer_format"], "JWT");

        let parsed: AuthScheme = serde_json::from_value(json).unwrap();
        match parsed {
            AuthScheme::Http { scheme, bearer_format } => {
                assert_eq!(scheme, "bearer");
                assert_eq!(bearer_format.as_deref(), Some("JWT"));
            }
            _ => panic!("expected Http variant"),
        }
    }

    #[test]
    fn http_scheme_omits_none_bearer_format() {
        let scheme = AuthScheme::Http {
            scheme: "basic".into(),
            bearer_format: None,
        };

        let json = serde_json::to_value(&scheme).unwrap();
        assert_eq!(json["type"], "http");
        assert_eq!(json["scheme"], "basic");
        assert!(json.get("bearer_format").is_none());
    }

    #[test]
    fn oauth2_scheme_tagged_serialization() {
        let mut scopes = HashMap::new();
        scopes.insert("read".into(), "Read access".into());
        scopes.insert("write".into(), "Write access".into());

        let scheme = AuthScheme::OAuth2 {
            grant_type: Some(OAuthGrantType::AuthorizationCode),
            authorization_url: Some("https://example.com/authorize".into()),
            token_url: Some("https://example.com/token".into()),
            scopes: Some(scopes),
        };

        let json = serde_json::to_value(&scheme).unwrap();
        assert_eq!(json["type"], "oauth2");
        assert_eq!(json["grant_type"], "authorization_code");
        assert_eq!(json["authorization_url"], "https://example.com/authorize");

        let parsed: AuthScheme = serde_json::from_value(json).unwrap();
        match parsed {
            AuthScheme::OAuth2 { grant_type, scopes, .. } => {
                assert_eq!(grant_type, Some(OAuthGrantType::AuthorizationCode));
                assert_eq!(scopes.as_ref().unwrap().len(), 2);
            }
            _ => panic!("expected OAuth2 variant"),
        }
    }

    #[test]
    fn oauth2_scheme_omits_none_fields() {
        let scheme = AuthScheme::OAuth2 {
            grant_type: None,
            authorization_url: None,
            token_url: None,
            scopes: None,
        };

        let json = serde_json::to_value(&scheme).unwrap();
        assert_eq!(json["type"], "oauth2");
        // All optional fields should be absent
        assert!(json.get("grant_type").is_none());
        assert!(json.get("authorization_url").is_none());
        assert!(json.get("token_url").is_none());
        assert!(json.get("scopes").is_none());
    }

    #[test]
    fn openid_connect_scheme_tagged_serialization() {
        let scheme = AuthScheme::OpenIdConnect {
            open_id_connect_url: "https://example.com/.well-known/openid-configuration".into(),
        };

        let json = serde_json::to_value(&scheme).unwrap();
        assert_eq!(json["type"], "openIdConnect");
        assert_eq!(
            json["open_id_connect_url"],
            "https://example.com/.well-known/openid-configuration"
        );

        let parsed: AuthScheme = serde_json::from_value(json).unwrap();
        match parsed {
            AuthScheme::OpenIdConnect { open_id_connect_url } => {
                assert_eq!(
                    open_id_connect_url,
                    "https://example.com/.well-known/openid-configuration"
                );
            }
            _ => panic!("expected OpenIdConnect variant"),
        }
    }
}
