//! The canonical memory repository.
//!
//! One logical writer per user namespace, an optimistic revision check, and a
//! whole-namespace materialization on every commit. Rewriting each affected
//! category file from the in-process record set (rather than patching files in
//! place) means a record that changes status — and therefore changes which file
//! it belongs in — cannot leave a stale copy behind.

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::document::OkfDocument;
use super::record::{from_document, to_document};
use super::store::{MemoryStore, OkfStore};
use crate::core::{
    CanonicalMemory, CanonicalPredicate, CommitReceipt, FactFingerprint, MemoryError, MemoryId,
    MemoryKind, MemoryStatus, UserId,
};

/// Manifest schema version.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// A single write in a transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryWrite {
    /// Create or replace a record.
    Put(Box<CanonicalMemory>),
    /// Remove a record and leave a content-free tombstone.
    Delete(MemoryId),
}

/// An all-or-nothing set of writes against one user's namespace.
///
/// A contradiction resolution writes two records — the new active fact and the
/// superseded old one. Committing only one of them would leave the corpus
/// asserting both, so transactions are the unit of commit.
#[derive(Debug, Clone)]
pub struct MemoryTransaction {
    /// Whose namespace is being written.
    pub user_id: UserId,
    /// The revision the caller read, if it is enforcing one.
    pub expected_revision: Option<u64>,
    /// The writes to apply, in order.
    pub writes: Vec<MemoryWrite>,
    /// Deduplication key so a retried commit is not applied twice.
    pub idempotency_key: String,
}

impl MemoryTransaction {
    /// An empty transaction for a user.
    pub fn new(user_id: UserId, idempotency_key: impl Into<String>) -> Self {
        Self {
            user_id,
            expected_revision: None,
            writes: Vec::new(),
            idempotency_key: idempotency_key.into(),
        }
    }

    /// Require the repository to still be at `revision`.
    pub fn expecting(mut self, revision: u64) -> Self {
        self.expected_revision = Some(revision);
        self
    }

    /// Add a record write.
    pub fn put(mut self, memory: CanonicalMemory) -> Self {
        self.writes.push(MemoryWrite::Put(Box::new(memory)));
        self
    }

    /// Add a deletion.
    pub fn delete(mut self, id: MemoryId) -> Self {
        self.writes.push(MemoryWrite::Delete(id));
        self
    }

    /// Whether there is anything to do.
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }
}

/// Which existing records a reconciliation should be compared against.
#[derive(Debug, Clone, Default)]
pub struct ReconciliationSelector {
    /// Exact fingerprint match.
    pub fingerprint: Option<FactFingerprint>,
    /// `subject|predicate` prefix match — the contradiction window.
    pub subject_predicate: Option<String>,
    /// Subject match — the wider window used to catch predicate drift.
    pub subject: Option<String>,
    /// Predicate match.
    pub predicate: Option<CanonicalPredicate>,
    /// Restrict to these kinds; empty means any.
    pub kinds: Vec<MemoryKind>,
    /// Restrict to these statuses; empty means active only.
    pub statuses: Vec<MemoryStatus>,
    /// Maximum records to return.
    pub limit: usize,
}

impl ReconciliationSelector {
    /// Look for the exact same fact.
    pub fn by_fingerprint(fingerprint: FactFingerprint) -> Self {
        Self {
            fingerprint: Some(fingerprint),
            limit: 8,
            ..Default::default()
        }
    }

    /// Look for anything asserted about the same subject and predicate.
    pub fn by_subject_predicate(prefix: impl Into<String>) -> Self {
        Self {
            subject_predicate: Some(prefix.into()),
            limit: 20,
            ..Default::default()
        }
    }

    /// Look for anything asserted about the same subject, whatever the
    /// predicate is called.
    ///
    /// Extraction models rename predicates between sessions; this is the window
    /// that lets reconciliation notice the same fact wearing a different name.
    pub fn by_subject(subject: impl Into<String>) -> Self {
        Self {
            subject: Some(subject.into()),
            limit: 40,
            ..Default::default()
        }
    }

    fn matches(&self, memory: &CanonicalMemory) -> bool {
        let status_ok = if self.statuses.is_empty() {
            memory.status == MemoryStatus::Active
        } else {
            self.statuses.contains(&memory.status)
        };
        if !status_ok {
            return false;
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&memory.kind) {
            return false;
        }
        if let Some(fp) = &self.fingerprint {
            if &memory.fingerprint() != fp {
                return false;
            }
        }
        if let Some(prefix) = &self.subject_predicate {
            if memory.fingerprint().subject_predicate() != prefix {
                return false;
            }
        }
        if let Some(subject) = &self.subject {
            if memory.fingerprint().subject() != subject.as_str() {
                return false;
            }
        }
        if let Some(predicate) = &self.predicate {
            if &memory.predicate != predicate {
                return false;
            }
        }
        true
    }
}

/// Read and write access to canonical memory.
#[async_trait]
pub trait MemoryRepository: Send + Sync {
    /// Fetch one record.
    async fn get(
        &self,
        user_id: &UserId,
        memory_id: &MemoryId,
    ) -> Result<Option<CanonicalMemory>, MemoryError>;

    /// Find records a proposal should be reconciled against.
    async fn find_candidates(
        &self,
        user_id: &UserId,
        selector: &ReconciliationSelector,
    ) -> Result<Vec<CanonicalMemory>, MemoryError>;

    /// Every record in the namespace, whatever its status.
    async fn all(&self, user_id: &UserId) -> Result<Vec<CanonicalMemory>, MemoryError>;

    /// The namespace's current revision.
    async fn revision(&self, user_id: &UserId) -> Result<u64, MemoryError>;

    /// Apply a transaction atomically.
    async fn commit(&self, transaction: MemoryTransaction) -> Result<CommitReceipt, MemoryError>;
}

/// A record's place in the manifest index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestEntry {
    /// Record identifier.
    pub id: MemoryId,
    /// The category file it lives in.
    pub path: String,
    /// Lifecycle state.
    pub status: MemoryStatus,
    /// Memory kind.
    pub kind: MemoryKind,
    /// Deduplication fingerprint.
    pub fingerprint: String,
}

/// A content-free record of a deletion.
///
/// Tombstones exist so a deleted memory cannot silently reappear from a stale
/// replica; they deliberately carry no statement text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tombstone {
    /// The record that was removed.
    pub id: MemoryId,
    /// When it was removed.
    pub deleted_at: DateTime<Utc>,
}

/// The index written alongside a user's records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Monotonic revision, bumped on every commit.
    pub revision: u64,
    /// One entry per record.
    pub records: Vec<ManifestEntry>,
    /// Deletions, without content.
    pub tombstones: Vec<Tombstone>,
}

impl Default for MemoryManifest {
    fn default() -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            revision: 0,
            records: Vec::new(),
            tombstones: Vec::new(),
        }
    }
}

/// Per-user in-process state, guarded by the namespace write lock.
#[derive(Debug, Default)]
struct Namespace {
    loaded: bool,
    revision: u64,
    records: BTreeMap<MemoryId, CanonicalMemory>,
    tombstones: Vec<Tombstone>,
    applied: Vec<String>,
    written_files: BTreeMap<String, String>,
}

/// An OKF repository projected onto an [`OkfStore`].
pub struct OkfRepository<S: OkfStore> {
    store: Arc<S>,
    namespaces: tokio::sync::Mutex<HashMap<UserId, Namespace>>,
}

impl OkfRepository<MemoryStore> {
    /// A repository backed by an in-process store.
    pub fn in_memory() -> Self {
        Self::new(Arc::new(MemoryStore::new()))
    }
}

impl<S: OkfStore> OkfRepository<S> {
    /// Wrap a store.
    pub fn new(store: Arc<S>) -> Self {
        Self {
            store,
            namespaces: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Borrow the underlying store.
    pub fn store(&self) -> &Arc<S> {
        &self.store
    }

    /// The directory prefix for a user.
    fn user_prefix(user_id: &UserId) -> String {
        format!("users/{}/", user_id.as_str())
    }

    fn manifest_path(user_id: &UserId) -> String {
        format!("{}manifest.json", Self::user_prefix(user_id))
    }

    async fn ensure_loaded(
        &self,
        namespaces: &mut HashMap<UserId, Namespace>,
        user_id: &UserId,
    ) -> Result<(), MemoryError> {
        let entry = namespaces.entry(user_id.clone()).or_default();
        if entry.loaded {
            return Ok(());
        }

        let prefix = Self::user_prefix(user_id);
        let manifest_path = Self::manifest_path(user_id);
        let manifest: MemoryManifest = match self.store.read(&manifest_path).await? {
            Some(raw) => serde_json::from_str(&raw).map_err(|e| MemoryError::MalformedRecord {
                path: manifest_path.clone(),
                message: e.to_string(),
            })?,
            None => MemoryManifest::default(),
        };

        let mut records = BTreeMap::new();
        let mut written_files = BTreeMap::new();
        for path in self.store.list(&prefix).await? {
            if !path.ends_with(".md") {
                continue;
            }
            let Some(contents) = self.store.read(&path).await? else {
                continue;
            };
            for doc in OkfDocument::parse_many(&contents, &path)? {
                let memory = from_document(&doc, &path)?;
                records.insert(memory.id.clone(), memory);
            }
            written_files.insert(path, contents);
        }

        *entry = Namespace {
            loaded: true,
            revision: manifest.revision,
            records,
            tombstones: manifest.tombstones,
            applied: Vec::new(),
            written_files,
        };
        Ok(())
    }

    /// Render the whole namespace to path → file contents.
    fn materialize(namespace: &Namespace) -> Result<BTreeMap<String, String>, MemoryError> {
        let mut grouped: BTreeMap<String, Vec<OkfDocument>> = BTreeMap::new();
        for memory in namespace.records.values() {
            let path = category_path(memory);
            grouped.entry(path).or_default().push(to_document(memory));
        }
        let mut out = BTreeMap::new();
        for (path, docs) in grouped {
            out.insert(path.clone(), OkfDocument::render_many(&docs, &path)?);
        }
        Ok(out)
    }
}

/// Which file a record belongs in (§25.1).
pub fn category_path(memory: &CanonicalMemory) -> String {
    let base = format!("users/{}/", memory.owner.as_str());
    match memory.status {
        MemoryStatus::Superseded | MemoryStatus::Expired => format!(
            "{base}superseded/{:04}-{:02}.md",
            memory.temporal.updated_at.year(),
            memory.temporal.updated_at.month()
        ),
        MemoryStatus::Staged => format!("{base}staged/patterns.md"),
        MemoryStatus::Deleted => format!("{base}tombstones/records.md"),
        MemoryStatus::Active => match memory.kind {
            MemoryKind::Identity => format!("{base}profile.md"),
            MemoryKind::Preference | MemoryKind::LocationPreference => {
                format!("{base}preferences.md")
            }
            MemoryKind::Relationship | MemoryKind::RelationshipPreference => {
                format!("{base}relationships.md")
            }
            MemoryKind::Routine => format!("{base}routines.md"),
            MemoryKind::Commitment => format!("{base}commitments.md"),
            MemoryKind::CommunicationStyle => format!("{base}communication.md"),
            MemoryKind::Project => format!("{base}projects.md"),
            MemoryKind::StagedPattern => format!("{base}staged/patterns.md"),
            MemoryKind::Episodic => format!(
                "{base}episodes/{}.md",
                memory.temporal.valid_from.format("%Y-%m-%d")
            ),
        },
    }
}

#[async_trait]
impl<S: OkfStore> MemoryRepository for OkfRepository<S> {
    async fn get(
        &self,
        user_id: &UserId,
        memory_id: &MemoryId,
    ) -> Result<Option<CanonicalMemory>, MemoryError> {
        let mut namespaces = self.namespaces.lock().await;
        self.ensure_loaded(&mut namespaces, user_id).await?;
        Ok(namespaces
            .get(user_id)
            .and_then(|ns| ns.records.get(memory_id))
            .cloned())
    }

    async fn find_candidates(
        &self,
        user_id: &UserId,
        selector: &ReconciliationSelector,
    ) -> Result<Vec<CanonicalMemory>, MemoryError> {
        let mut namespaces = self.namespaces.lock().await;
        self.ensure_loaded(&mut namespaces, user_id).await?;
        let namespace = namespaces.get(user_id).expect("namespace just loaded");
        let limit = if selector.limit == 0 {
            usize::MAX
        } else {
            selector.limit
        };
        Ok(namespace
            .records
            .values()
            .filter(|m| selector.matches(m))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn all(&self, user_id: &UserId) -> Result<Vec<CanonicalMemory>, MemoryError> {
        let mut namespaces = self.namespaces.lock().await;
        self.ensure_loaded(&mut namespaces, user_id).await?;
        Ok(namespaces
            .get(user_id)
            .map(|ns| ns.records.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn revision(&self, user_id: &UserId) -> Result<u64, MemoryError> {
        let mut namespaces = self.namespaces.lock().await;
        self.ensure_loaded(&mut namespaces, user_id).await?;
        Ok(namespaces.get(user_id).map(|ns| ns.revision).unwrap_or(0))
    }

    async fn commit(&self, transaction: MemoryTransaction) -> Result<CommitReceipt, MemoryError> {
        let mut namespaces = self.namespaces.lock().await;
        self.ensure_loaded(&mut namespaces, &transaction.user_id)
            .await?;
        let namespace = namespaces
            .get_mut(&transaction.user_id)
            .expect("namespace just loaded");

        // A retried commit returns the current revision rather than applying
        // the same writes again.
        if namespace.applied.contains(&transaction.idempotency_key) {
            return Ok(CommitReceipt {
                revision: namespace.revision,
                written: Vec::new(),
                deleted: Vec::new(),
            });
        }
        if let Some(expected) = transaction.expected_revision {
            if expected != namespace.revision {
                return Err(MemoryError::RevisionConflict {
                    expected,
                    actual: namespace.revision,
                });
            }
        }
        if transaction.is_empty() {
            namespace.applied.push(transaction.idempotency_key);
            return Ok(CommitReceipt {
                revision: namespace.revision,
                written: Vec::new(),
                deleted: Vec::new(),
            });
        }

        let mut written = Vec::new();
        let mut deleted = Vec::new();
        let now = Utc::now();
        for write in &transaction.writes {
            match write {
                MemoryWrite::Put(memory) => {
                    if memory.owner != transaction.user_id {
                        return Err(MemoryError::PolicyRefused(format!(
                            "record {} belongs to {}, not {}",
                            memory.id, memory.owner, transaction.user_id
                        )));
                    }
                    namespace
                        .records
                        .insert(memory.id.clone(), (**memory).clone());
                    written.push(memory.id.clone());
                }
                MemoryWrite::Delete(id) => {
                    if namespace.records.remove(id).is_some() {
                        namespace.tombstones.push(Tombstone {
                            id: id.clone(),
                            deleted_at: now,
                        });
                        deleted.push(id.clone());
                    }
                }
            }
        }

        let desired = Self::materialize(namespace)?;
        for (path, contents) in &desired {
            if namespace.written_files.get(path) != Some(contents) {
                self.store.write(path, contents).await?;
            }
        }
        for path in namespace.written_files.keys() {
            if !desired.contains_key(path) {
                self.store.remove(path).await?;
            }
        }

        namespace.revision += 1;
        namespace.written_files = desired;
        namespace.applied.push(transaction.idempotency_key);

        let manifest = MemoryManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            revision: namespace.revision,
            records: namespace
                .records
                .values()
                .map(|m| ManifestEntry {
                    id: m.id.clone(),
                    path: category_path(m),
                    status: m.status,
                    kind: m.kind,
                    fingerprint: m.fingerprint().to_string(),
                })
                .collect(),
            tombstones: namespace.tombstones.clone(),
        };
        self.store
            .write(
                &Self::manifest_path(&transaction.user_id),
                &serde_json::to_string_pretty(&manifest)?,
            )
            .await?;

        Ok(CommitReceipt {
            revision: namespace.revision,
            written,
            deleted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        EntityRef, EvidenceCounters, Explicitness, MemorySource, MemoryValue, PrivacyMetadata,
        RetrievalMetadata, SessionId, TemporalMetadata, TemporalScope, TurnId,
    };

    fn memory(id: &str, kind: MemoryKind, statement: &str) -> CanonicalMemory {
        let now = Utc::now();
        CanonicalMemory {
            id: MemoryId::new(id),
            owner: UserId::new("usr_1"),
            kind,
            predicate: CanonicalPredicate::new("dietary_identity"),
            status: MemoryStatus::Active,
            confidence: 0.9,
            subject: EntityRef::user(),
            value: MemoryValue::Text(statement.to_string()),
            statement: statement.to_string(),
            evidence_summary: "stated".into(),
            source: MemorySource::from_explicitness(
                Explicitness::ExplicitStatement,
                SessionId::new("ses_1"),
                TurnId(1),
            ),
            temporal: TemporalMetadata::created_at(now),
            retrieval: RetrievalMetadata {
                subject: "user".into(),
                ..Default::default()
            },
            evidence: EvidenceCounters::first(),
            privacy: PrivacyMetadata::default(),
            temporal_scope: TemporalScope::Persistent,
            supersedes: Vec::new(),
            superseded_by: None,
            qualifier: None,
        }
    }

    #[tokio::test]
    async fn commit_writes_records_and_reloads_them_from_the_store() {
        let store = Arc::new(MemoryStore::new());
        let repo = OkfRepository::new(store.clone());
        let user = UserId::new("usr_1");

        let receipt = repo
            .commit(MemoryTransaction::new(user.clone(), "tx-1").put(memory(
                "mem_a",
                MemoryKind::Preference,
                "The user is pescatarian.",
            )))
            .await
            .unwrap();
        assert_eq!(receipt.revision, 1);
        assert_eq!(receipt.written.len(), 1);

        // A fresh repository over the same store sees the same corpus.
        let reopened = OkfRepository::new(store.clone());
        let loaded = reopened.all(&user).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].statement, "The user is pescatarian.");
        assert_eq!(reopened.revision(&user).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn records_land_in_the_category_file_for_their_kind() {
        let store = Arc::new(MemoryStore::new());
        let repo = OkfRepository::new(store.clone());
        repo.commit(
            MemoryTransaction::new(UserId::new("usr_1"), "tx-1")
                .put(memory("mem_a", MemoryKind::Preference, "a"))
                .put(memory("mem_b", MemoryKind::Relationship, "b")),
        )
        .await
        .unwrap();

        let paths = store.paths();
        assert!(paths.contains(&"users/usr_1/preferences.md".to_string()));
        assert!(paths.contains(&"users/usr_1/relationships.md".to_string()));
    }

    #[tokio::test]
    async fn a_superseded_record_moves_out_of_its_active_file() {
        let store = Arc::new(MemoryStore::new());
        let repo = OkfRepository::new(store.clone());
        let user = UserId::new("usr_1");

        let mut old = memory("mem_old", MemoryKind::Preference, "The user is vegetarian.");
        repo.commit(MemoryTransaction::new(user.clone(), "tx-1").put(old.clone()))
            .await
            .unwrap();

        old.status = MemoryStatus::Superseded;
        old.superseded_by = Some(MemoryId::new("mem_new"));
        let new = memory(
            "mem_new",
            MemoryKind::Preference,
            "The user is pescatarian.",
        );
        repo.commit(
            MemoryTransaction::new(user.clone(), "tx-2")
                .put(old)
                .put(new),
        )
        .await
        .unwrap();

        let preferences = store
            .read("users/usr_1/preferences.md")
            .await
            .unwrap()
            .unwrap();
        assert!(preferences.contains("pescatarian"));
        assert!(
            !preferences.contains("vegetarian"),
            "superseded record must not linger in the active file"
        );
        assert!(store.paths().iter().any(|p| p.contains("superseded/")));
    }

    #[tokio::test]
    async fn deleting_a_record_removes_content_and_leaves_a_bare_tombstone() {
        let store = Arc::new(MemoryStore::new());
        let repo = OkfRepository::new(store.clone());
        let user = UserId::new("usr_1");

        repo.commit(MemoryTransaction::new(user.clone(), "tx-1").put(memory(
            "mem_a",
            MemoryKind::Preference,
            "secret preference",
        )))
        .await
        .unwrap();
        repo.commit(MemoryTransaction::new(user.clone(), "tx-2").delete(MemoryId::new("mem_a")))
            .await
            .unwrap();

        assert!(repo.all(&user).await.unwrap().is_empty());
        for path in store.paths() {
            let contents = store.read(&path).await.unwrap().unwrap_or_default();
            assert!(
                !contents.contains("secret preference"),
                "deleted content still present in {path}"
            );
        }
        let manifest: MemoryManifest = serde_json::from_str(
            &store
                .read("users/usr_1/manifest.json")
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.tombstones.len(), 1);
    }

    #[tokio::test]
    async fn a_retried_commit_is_applied_once() {
        let repo = OkfRepository::in_memory();
        let user = UserId::new("usr_1");
        let tx = || {
            MemoryTransaction::new(user.clone(), "same-key").put(memory(
                "mem_a",
                MemoryKind::Preference,
                "a",
            ))
        };
        let first = repo.commit(tx()).await.unwrap();
        let second = repo.commit(tx()).await.unwrap();
        assert_eq!(first.revision, second.revision);
        assert!(second.written.is_empty());
    }

    #[tokio::test]
    async fn a_stale_revision_is_refused() {
        let repo = OkfRepository::in_memory();
        let user = UserId::new("usr_1");
        repo.commit(MemoryTransaction::new(user.clone(), "tx-1").put(memory(
            "mem_a",
            MemoryKind::Preference,
            "a",
        )))
        .await
        .unwrap();

        let err = repo
            .commit(
                MemoryTransaction::new(user.clone(), "tx-2")
                    .expecting(0)
                    .put(memory("mem_b", MemoryKind::Preference, "b")),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::RevisionConflict { .. }));
    }

    #[tokio::test]
    async fn writing_into_another_users_namespace_is_refused() {
        let repo = OkfRepository::in_memory();
        let err = repo
            .commit(
                MemoryTransaction::new(UserId::new("usr_other"), "tx-1").put(memory(
                    "mem_a",
                    MemoryKind::Preference,
                    "a",
                )),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MemoryError::PolicyRefused(_)));
    }

    #[tokio::test]
    async fn candidates_are_selected_by_subject_and_predicate() {
        let repo = OkfRepository::in_memory();
        let user = UserId::new("usr_1");
        let target = memory("mem_a", MemoryKind::Preference, "vegetarian");
        let prefix = target.fingerprint().subject_predicate().to_string();
        repo.commit(MemoryTransaction::new(user.clone(), "tx-1").put(target))
            .await
            .unwrap();

        let found = repo
            .find_candidates(&user, &ReconciliationSelector::by_subject_predicate(prefix))
            .await
            .unwrap();
        assert_eq!(found.len(), 1);

        let none = repo
            .find_candidates(
                &user,
                &ReconciliationSelector::by_subject_predicate("user|something_else"),
            )
            .await
            .unwrap();
        assert!(none.is_empty());
    }
}
