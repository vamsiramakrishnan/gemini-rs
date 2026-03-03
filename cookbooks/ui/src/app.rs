use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppCategory {
    Basic,
    Advanced,
    Showcase,
}

impl std::fmt::Display for AppCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppCategory::Basic => write!(f, "Basic"),
            AppCategory::Advanced => write!(f, "Advanced"),
            AppCategory::Showcase => write!(f, "Showcase"),
        }
    }
}

/// Metadata about an app (sent to frontend for rendering cards).
#[derive(Debug, Clone, Serialize)]
pub struct AppInfo {
    pub name: String,
    pub description: String,
    pub category: AppCategory,
    pub features: Vec<String>,
    pub tips: Vec<String>,
    pub try_saying: Vec<String>,
}

/// Messages sent from app to the WebSocket handler to forward to the browser.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerMessage {
    Connected,
    TextDelta { text: String },
    TextComplete { text: String },
    Audio { data: String }, // base64
    TurnComplete,
    Interrupted,
    Error { message: String },
    InputTranscription { text: String },
    OutputTranscription { text: String },
    VoiceActivityStart,
    VoiceActivityEnd,
    // Devtools messages
    StateUpdate { key: String, value: serde_json::Value },
    PhaseChange { from: String, to: String, reason: String },
    Evaluation { phase: String, score: f64, notes: String },
    Violation { rule: String, severity: String, detail: String },
    AppMeta { info: AppInfo },
}

/// Messages received from the browser.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientMessage {
    Start {
        #[serde(default)]
        system_instruction: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        voice: Option<String>,
    },
    Text { text: String },
    Audio { data: String }, // base64
    Stop,
}

/// Sender handle for sending messages to the browser.
pub type WsSender = mpsc::UnboundedSender<ServerMessage>;

/// The trait that all cookbook apps implement.
#[async_trait]
pub trait CookbookApp: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn category(&self) -> AppCategory;
    fn features(&self) -> Vec<String>;
    fn tips(&self) -> Vec<String> { Vec::new() }
    fn try_saying(&self) -> Vec<String> { Vec::new() }

    /// Handle a full WebSocket session. Called when a client connects to /ws/<name>.
    /// The app receives client messages via rx and sends server messages via tx.
    async fn handle_session(
        &self,
        tx: WsSender,
        rx: mpsc::UnboundedReceiver<ClientMessage>,
    ) -> Result<(), AppError>;
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Connection error: {0}")]
    Connection(String),
    #[error("Session error: {0}")]
    Session(String),
    #[error("{0}")]
    Other(String),
}

/// Registry of all available cookbook apps.
pub struct AppRegistry {
    apps: HashMap<String, Arc<dyn CookbookApp>>,
    order: Vec<String>, // preserve insertion order for display
}

impl AppRegistry {
    pub fn new() -> Self {
        Self {
            apps: HashMap::new(),
            order: Vec::new(),
        }
    }

    pub fn register(&mut self, app: impl CookbookApp + 'static) {
        let name = app.name().to_string();
        self.order.push(name.clone());
        self.apps.insert(name, Arc::new(app));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn CookbookApp>> {
        self.apps.get(name).cloned()
    }

    pub fn list(&self) -> Vec<AppInfo> {
        self.order
            .iter()
            .filter_map(|name| self.apps.get(name))
            .map(|app| AppInfo {
                name: app.name().to_string(),
                description: app.description().to_string(),
                category: app.category(),
                features: app.features(),
                tips: app.tips(),
                try_saying: app.try_saying(),
            })
            .collect()
    }
}
