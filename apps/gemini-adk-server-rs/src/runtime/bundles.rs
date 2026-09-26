//! The bundles a runtime serves, re-resolved from the store over time.

use std::collections::BTreeMap;
use std::sync::Arc;

use gemini_adk_fluent_rs::spec::{BundleRef, BundleStore, BundleVersion, SessionSpec, StoreError};
use parking_lot::RwLock;

/// After this many refreshes in a row fail to reach the store, `/readyz`
/// reports not ready. One failure is usually a blip; the bundles already
/// loaded keep serving either way.
pub(crate) const STORE_FAILURES_BEFORE_UNREADY: u32 = 3;

/// One bundle version, loaded and ready to start sessions from.
#[derive(Debug)]
pub struct LoadedBundle {
    /// The bundle's name (the route key).
    pub name: String,
    /// The reference it was loaded by, such as `booking:prod`.
    pub reference: String,
    /// The version the reference resolved to.
    pub version: BundleVersion,
    /// The spec.
    pub spec: SessionSpec,
}

/// What [`Runtime::refresh`](super::Runtime::refresh) changed, and what failed.
#[derive(Debug, Clone, Default)]
pub struct RefreshReport {
    /// `(reference, old version, new version)` for every reference that now
    /// resolves to a different version (old is `None` on first load).
    pub changed: Vec<(String, Option<String>, String)>,
    /// One message per reference that could not be resolved or loaded.
    pub errors: Vec<String>,
}

struct Served {
    reference: String,
    name: String,
    by: BundleRef,
}

#[derive(Default)]
struct Health {
    store_failures: u32,
    errors: BTreeMap<String, String>,
}

/// The configured references and what they currently resolve to.
pub(crate) struct BundleSet {
    store: Arc<dyn BundleStore>,
    served: Vec<Served>,
    loaded: RwLock<BTreeMap<String, Arc<LoadedBundle>>>,
    health: RwLock<Health>,
}

impl BundleSet {
    pub(crate) fn new(store: Arc<dyn BundleStore>, references: &[String]) -> Self {
        let served = references
            .iter()
            .map(|reference| {
                let (name, by) = BundleRef::parse(reference);
                Served {
                    reference: reference.clone(),
                    name,
                    by,
                }
            })
            .collect();
        Self {
            store,
            served,
            loaded: RwLock::new(BTreeMap::new()),
            health: RwLock::new(Health::default()),
        }
    }

    /// The bundle currently served under `name`. A session keeps the `Arc`
    /// it started with, so moving a label never changes a running session.
    pub(crate) fn get(&self, name: &str) -> Option<Arc<LoadedBundle>> {
        self.loaded.read().get(name).cloned()
    }

    /// Every loaded bundle, by name.
    pub(crate) fn loaded(&self) -> Vec<Arc<LoadedBundle>> {
        self.loaded.read().values().cloned().collect()
    }

    /// Resolve every reference again and swap in versions that changed. A
    /// reference that fails keeps its current version.
    pub(crate) async fn refresh(&self) -> RefreshReport {
        let mut report = RefreshReport::default();
        let mut store_failed = false;
        let mut errors = BTreeMap::new();
        for served in &self.served {
            let result =
                self.store
                    .get(&served.name, &served.by)
                    .await
                    .and_then(|(version, spec)| {
                        unsupported(&spec).map_or(Ok((version, spec)), |why| {
                            Err(StoreError::Invalid(vec![why]))
                        })
                    });
            match result {
                Ok((version, spec)) => {
                    let previous = self.get(&served.name).map(|b| b.version.version.clone());
                    if previous.as_deref() != Some(version.version.as_str()) {
                        report.changed.push((
                            served.reference.clone(),
                            previous,
                            version.version.clone(),
                        ));
                        self.loaded.write().insert(
                            served.name.clone(),
                            Arc::new(LoadedBundle {
                                name: served.name.clone(),
                                reference: served.reference.clone(),
                                version,
                                spec,
                            }),
                        );
                    }
                }
                Err(e) => {
                    store_failed |= matches!(e, StoreError::Backend(_));
                    let message = format!("{}: {e}", served.reference);
                    report.errors.push(message.clone());
                    errors.insert(served.reference.clone(), message);
                }
            }
        }
        let mut health = self.health.write();
        health.store_failures = if store_failed {
            health.store_failures.saturating_add(1)
        } else {
            0
        };
        health.errors = errors;
        report
    }

    /// Why the set is not ready to serve, or an empty list when it is.
    pub(crate) fn not_ready_reasons(&self) -> Vec<String> {
        let loaded = self.loaded.read();
        let health = self.health.read();
        let mut reasons: Vec<String> = self
            .served
            .iter()
            .filter(|s| !loaded.contains_key(&s.name))
            .map(|s| {
                health
                    .errors
                    .get(&s.reference)
                    .cloned()
                    .unwrap_or_else(|| format!("{}: not loaded yet", s.reference))
            })
            .collect();
        if health.store_failures >= STORE_FAILURES_BEFORE_UNREADY {
            reasons.push(format!(
                "bundle store unreachable for {} refreshes in a row",
                health.store_failures
            ));
        }
        reasons
    }
}

/// What this runtime cannot provide for a spec, if anything.
fn unsupported(spec: &SessionSpec) -> Option<String> {
    spec.memory.is_some().then(|| {
        "the spec declares `memory`, and adk-runtime has no memory engine; \
         serve it from your own server with SpecResources::memory"
            .to_string()
    })
}
