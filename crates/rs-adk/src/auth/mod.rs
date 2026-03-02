//! Auth module — credential types, security schemes, and auth configuration.
//!
//! This module provides the full ADK-JS-compatible authentication type hierarchy:
//! - [`AuthCredential`] and [`AuthCredentialType`] — credential storage
//! - [`AuthScheme`] — OpenAPI 3.0-style security scheme definitions
//! - [`AuthConfig`] — binds a scheme to credentials
//! - [`AuthToolArguments`] — passed to tools when auth is required

pub mod config;
pub mod credential;
pub mod schemes;

pub use config::{AuthConfig, AuthToolArguments};
pub use credential::{
    AuthCredential, AuthCredentialType, HttpAuth, HttpCredentials, OAuth2Auth,
    ServiceAccountCredential,
};
pub use schemes::{AuthScheme, OAuthGrantType};
