//! M — Middleware composition.
//!
//! Compose middleware in any order with `|`.

use std::sync::Arc;
use std::time::Duration;

use rs_adk::middleware::{LatencyMiddleware, LogMiddleware, Middleware};

/// A middleware composite — one or more middleware layers.
#[derive(Clone)]
pub struct MiddlewareComposite {
    pub layers: Vec<Arc<dyn Middleware>>,
}

impl MiddlewareComposite {
    pub fn new(layer: Arc<dyn Middleware>) -> Self {
        Self {
            layers: vec![layer],
        }
    }

    /// Number of layers.
    pub fn len(&self) -> usize {
        self.layers.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

/// Compose two middleware composites with `|`.
impl std::ops::BitOr for MiddlewareComposite {
    type Output = MiddlewareComposite;

    fn bitor(mut self, rhs: MiddlewareComposite) -> Self::Output {
        self.layers.extend(rhs.layers);
        self
    }
}

/// The `M` namespace — static factory methods for middleware.
pub struct M;

impl M {
    /// Add logging middleware.
    pub fn log() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(LogMiddleware::new()))
    }

    /// Add latency tracking middleware.
    pub fn latency() -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(LatencyMiddleware::new()))
    }

    /// Add timeout middleware (placeholder — records the duration for use by the runtime).
    pub fn timeout(duration: Duration) -> MiddlewareComposite {
        MiddlewareComposite::new(Arc::new(TimeoutMiddleware {
            name: "timeout".to_string(),
            duration,
        }))
    }
}

/// Timeout middleware — stores the configured duration for runtime enforcement.
#[allow(dead_code)]
struct TimeoutMiddleware {
    name: String,
    duration: Duration,
}

#[async_trait::async_trait]
impl Middleware for TimeoutMiddleware {
    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_creates_composite() {
        let m = M::log();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn latency_creates_composite() {
        let m = M::latency();
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn timeout_creates_composite() {
        let m = M::timeout(Duration::from_secs(30));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn compose_with_bitor() {
        let m = M::log() | M::latency() | M::timeout(Duration::from_secs(5));
        assert_eq!(m.len(), 3);
    }
}
