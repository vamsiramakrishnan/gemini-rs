//! Runtime configuration, read from the environment.

use std::time::Duration;

use gemini_adk_fluent_rs::spec::BundleRef;

/// How `adk-runtime` is configured. See [`RuntimeConfig::from_env`] for the
/// variables and their defaults.
#[derive(Clone)]
pub struct RuntimeConfig {
    /// Port to listen on, on every interface (`PORT`, default 8080).
    pub port: u16,
    /// The bundle store: a directory or `gs://bucket/prefix` (`ADK_BUNDLES`).
    pub store: String,
    /// The bundles to serve, such as `booking:prod` (`ADK_SERVE`).
    pub serve: Vec<String>,
    /// Bearer tokens or API keys accepted on session endpoints
    /// (`ADK_RUNTIME_TOKENS`).
    pub tokens: Vec<String>,
    /// Serve session endpoints without a token (`ADK_RUNTIME_INSECURE=1`).
    pub insecure: bool,
    /// Twilio's auth token; enables the Twilio routes (`TWILIO_AUTH_TOKEN`).
    pub twilio_auth_token: Option<String>,
    /// The URL clients reach this service at, such as
    /// `https://booking-abc123.a.run.app` (`ADK_PUBLIC_URL`). Twilio signs
    /// the URL it called, so set this when a proxy rewrites the host.
    pub public_url: Option<String>,
    /// Sessions served at once before new ones get 503 (`ADK_MAX_SESSIONS`).
    pub max_sessions: usize,
    /// Longest a session may run (`ADK_MAX_SESSION_SECS`).
    pub max_session: Duration,
    /// Largest WebSocket message accepted (`ADK_MAX_MESSAGE_BYTES`).
    pub max_message_bytes: usize,
    /// How often labels are re-resolved (`ADK_REFRESH_SECS`).
    pub refresh_every: Duration,
    /// How long running sessions may continue after SIGTERM
    /// (`ADK_DRAIN_SECS`).
    pub drain_grace: Duration,
    /// Live model for every session (`GEMINI_LIVE_MODEL`); unset uses the
    /// platform default.
    pub model: Option<String>,
}

impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfig")
            .field("port", &self.port)
            .field("store", &self.store)
            .field("serve", &self.serve)
            .field("tokens", &format_args!("<{} redacted>", self.tokens.len()))
            .field("insecure", &self.insecure)
            .field(
                "twilio_auth_token",
                &self.twilio_auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field("public_url", &self.public_url)
            .field("max_sessions", &self.max_sessions)
            .field("max_session", &self.max_session)
            .field("max_message_bytes", &self.max_message_bytes)
            .field("refresh_every", &self.refresh_every)
            .field("drain_grace", &self.drain_grace)
            .field("model", &self.model)
            .finish()
    }
}

impl RuntimeConfig {
    /// A configuration for `store` serving `serve`, with the defaults below
    /// and no authentication (which [`check`](Self::check) refuses).
    pub fn new(
        store: impl Into<String>,
        serve: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            port: 8080,
            store: store.into(),
            serve: serve.into_iter().map(Into::into).collect(),
            tokens: Vec::new(),
            insecure: false,
            twilio_auth_token: None,
            public_url: None,
            max_sessions: 50,
            max_session: Duration::from_secs(3600),
            max_message_bytes: 256 * 1024,
            refresh_every: Duration::from_secs(30),
            drain_grace: Duration::from_secs(8),
            model: None,
        }
    }

    /// Read the configuration from the environment:
    ///
    /// | Variable | Default | Meaning |
    /// |---|---|---|
    /// | `ADK_BUNDLES` | required | bundle store URI |
    /// | `ADK_SERVE` | required | comma-separated references, e.g. `booking:prod,clinic:prod` |
    /// | `ADK_RUNTIME_TOKENS` | none | comma-separated bearer tokens / API keys |
    /// | `ADK_RUNTIME_INSECURE` | off | `1` serves session endpoints without a token |
    /// | `TWILIO_AUTH_TOKEN` | none | enables `/twilio/*` |
    /// | `ADK_PUBLIC_URL` | from request headers | public base URL |
    /// | `PORT` | 8080 | listen port |
    /// | `ADK_MAX_SESSIONS` | 50 | concurrent sessions |
    /// | `ADK_MAX_SESSION_SECS` | 3600 | session length limit |
    /// | `ADK_MAX_MESSAGE_BYTES` | 262144 | WebSocket message limit |
    /// | `ADK_REFRESH_SECS` | 30 | label re-resolution interval |
    /// | `ADK_DRAIN_SECS` | 8 | grace period after SIGTERM |
    /// | `GEMINI_LIVE_MODEL` | platform default | Live model |
    ///
    /// The result is also [`check`](Self::check)ed.
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// [`from_env`](Self::from_env) over any variable lookup, for tests.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let get = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let list = |key: &str| -> Vec<String> {
            get(key)
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        fn number<T: std::str::FromStr>(
            key: &str,
            value: Option<String>,
            default: T,
        ) -> Result<T, String> {
            match value {
                None => Ok(default),
                Some(v) => v
                    .parse()
                    .map_err(|_| format!("{key}={v} is not a valid number")),
            }
        }

        let store = get("ADK_BUNDLES")
            .ok_or("ADK_BUNDLES is required: a directory or gs://bucket/prefix")?;
        let mut config = Self::new(store, list("ADK_SERVE"));
        config.tokens = list("ADK_RUNTIME_TOKENS");
        config.insecure = matches!(
            get("ADK_RUNTIME_INSECURE").as_deref(),
            Some("1" | "true" | "TRUE" | "yes")
        );
        config.twilio_auth_token = get("TWILIO_AUTH_TOKEN");
        config.public_url = get("ADK_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_string());
        config.port = number("PORT", get("PORT"), config.port)?;
        config.max_sessions = number(
            "ADK_MAX_SESSIONS",
            get("ADK_MAX_SESSIONS"),
            config.max_sessions,
        )?;
        config.max_session = Duration::from_secs(number(
            "ADK_MAX_SESSION_SECS",
            get("ADK_MAX_SESSION_SECS"),
            config.max_session.as_secs(),
        )?);
        config.max_message_bytes = number(
            "ADK_MAX_MESSAGE_BYTES",
            get("ADK_MAX_MESSAGE_BYTES"),
            config.max_message_bytes,
        )?;
        config.refresh_every = Duration::from_secs(number(
            "ADK_REFRESH_SECS",
            get("ADK_REFRESH_SECS"),
            config.refresh_every.as_secs(),
        )?);
        config.drain_grace = Duration::from_secs(number(
            "ADK_DRAIN_SECS",
            get("ADK_DRAIN_SECS"),
            config.drain_grace.as_secs(),
        )?);
        config.model = get("GEMINI_LIVE_MODEL");
        config.check()?;
        Ok(config)
    }

    /// Refuse a configuration the runtime cannot serve safely:
    ///
    /// - no bundles to serve, or two references to the same bundle name
    ///   (the name is the route);
    /// - no authentication at all (neither `ADK_RUNTIME_TOKENS` nor
    ///   `TWILIO_AUTH_TOKEN`) unless `ADK_RUNTIME_INSECURE=1`;
    /// - a zero session cap, session length, message limit or refresh
    ///   interval.
    pub fn check(&self) -> Result<(), String> {
        if self.serve.is_empty() {
            return Err(
                "ADK_SERVE is required: comma-separated bundle references such as booking:prod"
                    .into(),
            );
        }
        let mut names = Vec::new();
        for reference in &self.serve {
            let (name, _) = BundleRef::parse(reference);
            if names.contains(&name) {
                return Err(format!(
                    "ADK_SERVE names bundle '{name}' twice; each bundle is served under its name, once"
                ));
            }
            names.push(name);
        }
        if self.tokens.is_empty() && self.twilio_auth_token.is_none() && !self.insecure {
            return Err(
                "no authentication configured: set ADK_RUNTIME_TOKENS (and/or TWILIO_AUTH_TOKEN), \
                 or ADK_RUNTIME_INSECURE=1 to serve without it"
                    .into(),
            );
        }
        if self.max_sessions == 0
            || self.max_session.is_zero()
            || self.max_message_bytes == 0
            || self.refresh_every.is_zero()
        {
            return Err(
                "ADK_MAX_SESSIONS, ADK_MAX_SESSION_SECS, ADK_MAX_MESSAGE_BYTES and \
                 ADK_REFRESH_SECS must be greater than zero"
                    .into(),
            );
        }
        Ok(())
    }

    /// Whether session endpoints are open without a token.
    pub(crate) fn open_sessions(&self) -> bool {
        self.tokens.is_empty() && self.insecure
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| vars.get(key).cloned()
    }

    const BASE: [(&str, &str); 2] = [
        ("ADK_BUNDLES", "/srv/bundles"),
        ("ADK_SERVE", "booking:prod, clinic:prod"),
    ];

    #[test]
    fn no_authentication_refuses_to_start() {
        let err = RuntimeConfig::from_lookup(lookup(&BASE)).unwrap_err();
        assert!(err.contains("no authentication configured"), "{err}");
    }

    #[test]
    fn insecure_mode_starts_without_tokens() {
        let mut vars = BASE.to_vec();
        vars.push(("ADK_RUNTIME_INSECURE", "1"));
        let config = RuntimeConfig::from_lookup(lookup(&vars)).unwrap();
        assert!(config.open_sessions());
    }

    #[test]
    fn tokens_or_twilio_alone_are_enough() {
        let mut vars = BASE.to_vec();
        vars.push(("ADK_RUNTIME_TOKENS", "a, b,"));
        let config = RuntimeConfig::from_lookup(lookup(&vars)).unwrap();
        assert_eq!(config.tokens, ["a", "b"]);
        assert_eq!(config.serve, ["booking:prod", "clinic:prod"]);
        assert!(!config.open_sessions());

        let mut vars = BASE.to_vec();
        vars.push(("TWILIO_AUTH_TOKEN", "t"));
        assert!(RuntimeConfig::from_lookup(lookup(&vars)).is_ok());
    }

    #[test]
    fn defaults_follow_cloud_run_conventions() {
        let mut vars = BASE.to_vec();
        vars.push(("ADK_RUNTIME_TOKENS", "a"));
        let config = RuntimeConfig::from_lookup(lookup(&vars)).unwrap();
        assert_eq!(config.port, 8080);
        assert!(config.drain_grace < Duration::from_secs(10));

        vars.push(("PORT", "9000"));
        vars.push(("ADK_MAX_SESSIONS", "3"));
        let config = RuntimeConfig::from_lookup(lookup(&vars)).unwrap();
        assert_eq!((config.port, config.max_sessions), (9000, 3));
    }

    #[test]
    fn bad_values_are_named() {
        let mut vars = BASE.to_vec();
        vars.push(("ADK_RUNTIME_TOKENS", "a"));
        vars.push(("ADK_MAX_SESSIONS", "lots"));
        let err = RuntimeConfig::from_lookup(lookup(&vars)).unwrap_err();
        assert!(err.contains("ADK_MAX_SESSIONS=lots"), "{err}");

        let vars = [
            ("ADK_BUNDLES", "b"),
            ("ADK_SERVE", "booking:prod,booking:staging"),
            ("ADK_RUNTIME_TOKENS", "a"),
        ];
        let err = RuntimeConfig::from_lookup(lookup(&vars)).unwrap_err();
        assert!(err.contains("twice"), "{err}");
    }

    #[test]
    fn debug_output_hides_secrets() {
        let mut config = RuntimeConfig::new("b", ["x"]);
        config.tokens = vec!["super-secret".into()];
        config.twilio_auth_token = Some("also-secret".into());
        let shown = format!("{config:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }
}
