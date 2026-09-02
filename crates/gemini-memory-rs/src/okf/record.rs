//! Mapping between [`CanonicalMemory`] and its on-disk OKF representation.
//!
//! This is the only place that knows the file format. Everything else in the
//! engine works with the domain type, so changing the serialization is a local
//! edit rather than a sweep.

use chrono::{DateTime, Utc};

use super::document::OkfDocument;
use super::yaml::{self, Yaml};
use crate::core::{
    CanonicalMemory, CanonicalPredicate, EntityRef, EvidenceCounters, MemoryError, MemoryId,
    MemoryKind, MemorySource, MemoryStatus, MemoryValue, PrivacyMetadata, RetrievalMetadata,
    SensitivityClass, SessionId, TemporalMetadata, TemporalScope, TurnId, UserId, ids::EntityId,
};

/// The format version written into every record.
pub const OKF_VERSION: &str = "memory/v1";

/// Section headings, in emission order.
const SECTION_FACT: &str = "Fact";
const SECTION_EVIDENCE: &str = "Evidence Summary";
const SECTION_SUPERSEDES: &str = "Supersedes";

/// Render a canonical memory as an OKF document.
pub fn to_document(memory: &CanonicalMemory) -> OkfDocument {
    let front = yaml::map(vec![
        ("okf", Some(OKF_VERSION.into())),
        ("id", Some(memory.id.as_str().into())),
        ("owner", Some(memory.owner.as_str().into())),
        ("kind", Some(memory.kind.to_string().into())),
        ("predicate", Some(memory.predicate.as_str().into())),
        ("status", Some(status_label(memory.status).into())),
        (
            "confidence",
            Some(Yaml::Float(f64::from(memory.confidence))),
        ),
        (
            "temporal_scope",
            Some(temporal_scope_label(memory.temporal_scope).into()),
        ),
        (
            "subject",
            Some(yaml::map(vec![
                ("id", Some(memory.subject.id.as_str().into())),
                ("display", Some(memory.subject.display.clone().into())),
                (
                    "aliases",
                    Some(yaml::seq_of_strings(memory.subject.aliases.clone())),
                ),
            ])),
        ),
        ("value", Some(value_to_yaml(&memory.value))),
        (
            "qualifier",
            Some(
                memory
                    .qualifier
                    .clone()
                    .map(Yaml::Str)
                    .unwrap_or(Yaml::Null),
            ),
        ),
        (
            "source",
            Some(yaml::map(vec![
                ("type", Some(memory.source.source_type.clone().into())),
                (
                    "session_id",
                    Some(
                        memory
                            .source
                            .session_id
                            .as_ref()
                            .map(|s| Yaml::Str(s.to_string()))
                            .unwrap_or(Yaml::Null),
                    ),
                ),
                (
                    "turn_id",
                    Some(
                        memory
                            .source
                            .turn_id
                            .map(|t| Yaml::Str(t.to_string()))
                            .unwrap_or(Yaml::Null),
                    ),
                ),
            ])),
        ),
        (
            "temporal",
            Some(yaml::map(vec![
                ("created_at", Some(timestamp(memory.temporal.created_at))),
                ("updated_at", Some(timestamp(memory.temporal.updated_at))),
                (
                    "last_confirmed_at",
                    Some(timestamp(memory.temporal.last_confirmed_at)),
                ),
                ("valid_from", Some(timestamp(memory.temporal.valid_from))),
                (
                    "valid_to",
                    Some(optional_timestamp(memory.temporal.valid_to)),
                ),
                (
                    "expires_at",
                    Some(optional_timestamp(memory.temporal.expires_at)),
                ),
            ])),
        ),
        (
            "retrieval",
            Some(yaml::map(vec![
                ("subject", Some(memory.retrieval.subject.clone().into())),
                (
                    "tags",
                    Some(yaml::seq_of_strings(memory.retrieval.tags.clone())),
                ),
                (
                    "aliases",
                    Some(yaml::seq_of_strings(memory.retrieval.aliases.clone())),
                ),
                (
                    "entities",
                    Some(yaml::seq_of_strings(memory.retrieval.entities.clone())),
                ),
                (
                    "location",
                    Some(
                        memory
                            .retrieval
                            .location
                            .clone()
                            .map(Yaml::Str)
                            .unwrap_or(Yaml::Null),
                    ),
                ),
            ])),
        ),
        (
            "evidence",
            Some(yaml::map(vec![
                ("count", Some(memory.evidence.count.into())),
                (
                    "distinct_sessions",
                    Some(memory.evidence.distinct_sessions.into()),
                ),
                ("distinct_days", Some(memory.evidence.distinct_days.into())),
            ])),
        ),
        (
            "privacy",
            Some(yaml::map(vec![
                ("deletable", Some(memory.privacy.deletable.into())),
                ("exportable", Some(memory.privacy.exportable.into())),
                (
                    "sensitivity",
                    Some(sensitivity_label(memory.privacy.sensitivity).into()),
                ),
            ])),
        ),
        (
            "superseded_by",
            Some(
                memory
                    .superseded_by
                    .as_ref()
                    .map(|m| Yaml::Str(m.to_string()))
                    .unwrap_or(Yaml::Null),
            ),
        ),
    ]);

    let mut sections = vec![
        (SECTION_FACT, memory.statement.clone()),
        (SECTION_EVIDENCE, memory.evidence_summary.clone()),
    ];
    if !memory.supersedes.is_empty() {
        let list = memory
            .supersedes
            .iter()
            .map(|id| format!("- {id}"))
            .collect::<Vec<_>>()
            .join("\n");
        sections.push((SECTION_SUPERSEDES, list));
    }
    OkfDocument::new(front, sections)
}

/// Parse an OKF document back into a canonical memory.
pub fn from_document(doc: &OkfDocument, path: &str) -> Result<CanonicalMemory, MemoryError> {
    let err = |message: String| MemoryError::MalformedRecord {
        path: path.to_string(),
        message,
    };

    let version = doc
        .front
        .get("okf")
        .and_then(Yaml::as_string)
        .ok_or_else(|| err("missing `okf` version".to_string()))?;
    if version != OKF_VERSION {
        return Err(err(format!("unsupported OKF version `{version}`")));
    }

    let id = MemoryId::new(
        doc.front
            .get("id")
            .and_then(Yaml::as_string)
            .ok_or_else(|| err("missing `id`".to_string()))?,
    );
    let owner = UserId::new(
        doc.front
            .get("owner")
            .and_then(Yaml::as_string)
            .ok_or_else(|| err("missing `owner`".to_string()))?,
    );
    let kind = parse_kind(
        &doc.front
            .get("kind")
            .and_then(Yaml::as_string)
            .ok_or_else(|| err("missing `kind`".to_string()))?,
    )
    .ok_or_else(|| err("unknown `kind`".to_string()))?;
    let predicate = CanonicalPredicate::new(
        doc.front
            .get("predicate")
            .and_then(Yaml::as_string)
            .ok_or_else(|| err("missing `predicate`".to_string()))?,
    );
    let status = parse_status(
        &doc.front
            .get("status")
            .and_then(Yaml::as_string)
            .unwrap_or_else(|| "active".to_string()),
    )
    .ok_or_else(|| err("unknown `status`".to_string()))?;
    let confidence = doc
        .front
        .get("confidence")
        .and_then(Yaml::as_f64)
        .unwrap_or(0.0) as f32;
    let temporal_scope = doc
        .front
        .get("temporal_scope")
        .and_then(Yaml::as_string)
        .and_then(|s| parse_temporal_scope(&s))
        .unwrap_or(TemporalScope::Persistent);

    let subject_node = doc.front.get("subject");
    let subject = match subject_node {
        Some(node) if node.get("display").is_some() => EntityRef {
            id: EntityId::new(
                node.get("id")
                    .and_then(Yaml::as_string)
                    .unwrap_or_else(|| "user".to_string()),
            ),
            display: node
                .get("display")
                .and_then(Yaml::as_string)
                .unwrap_or_else(|| "user".to_string()),
            aliases: node
                .get("aliases")
                .map(Yaml::as_string_list)
                .unwrap_or_default(),
        },
        _ => EntityRef::user(),
    };

    let value = doc
        .front
        .get("value")
        .map(yaml_to_value)
        .transpose()
        .map_err(err)?
        .unwrap_or_else(|| MemoryValue::Text(String::new()));

    let source_node = doc.front.get("source");
    let source = MemorySource {
        source_type: source_node
            .and_then(|n| n.get("type"))
            .and_then(Yaml::as_string)
            .unwrap_or_else(|| "unknown".to_string()),
        session_id: source_node
            .and_then(|n| n.get("session_id"))
            .and_then(Yaml::as_string)
            .map(SessionId::new),
        turn_id: source_node
            .and_then(|n| n.get("turn_id"))
            .and_then(Yaml::as_string)
            .and_then(|raw| parse_turn_id(&raw)),
    };

    let temporal_node = doc
        .front
        .get("temporal")
        .ok_or_else(|| err("missing `temporal` block".to_string()))?;
    let created_at = required_time(temporal_node, "created_at", path)?;
    let temporal = TemporalMetadata {
        created_at,
        updated_at: optional_time(temporal_node, "updated_at").unwrap_or(created_at),
        last_confirmed_at: optional_time(temporal_node, "last_confirmed_at").unwrap_or(created_at),
        valid_from: optional_time(temporal_node, "valid_from").unwrap_or(created_at),
        valid_to: optional_time(temporal_node, "valid_to"),
        expires_at: optional_time(temporal_node, "expires_at"),
    };

    let retrieval_node = doc.front.get("retrieval");
    let retrieval = RetrievalMetadata {
        subject: retrieval_node
            .and_then(|n| n.get("subject"))
            .and_then(Yaml::as_string)
            .unwrap_or_else(|| subject.display.clone()),
        tags: retrieval_node
            .and_then(|n| n.get("tags"))
            .map(Yaml::as_string_list)
            .unwrap_or_default(),
        aliases: retrieval_node
            .and_then(|n| n.get("aliases"))
            .map(Yaml::as_string_list)
            .unwrap_or_default(),
        entities: retrieval_node
            .and_then(|n| n.get("entities"))
            .map(Yaml::as_string_list)
            .unwrap_or_default(),
        location: retrieval_node
            .and_then(|n| n.get("location"))
            .and_then(Yaml::as_string),
    };

    let evidence_node = doc.front.get("evidence");
    let evidence = EvidenceCounters {
        count: evidence_node
            .and_then(|n| n.get("count"))
            .and_then(Yaml::as_u64)
            .unwrap_or(1) as u32,
        distinct_sessions: evidence_node
            .and_then(|n| n.get("distinct_sessions"))
            .and_then(Yaml::as_u64)
            .unwrap_or(1) as u32,
        distinct_days: evidence_node
            .and_then(|n| n.get("distinct_days"))
            .and_then(Yaml::as_u64)
            .unwrap_or(1) as u32,
    };

    let privacy_node = doc.front.get("privacy");
    let privacy = PrivacyMetadata {
        deletable: privacy_node
            .and_then(|n| n.get("deletable"))
            .and_then(Yaml::as_bool)
            .unwrap_or(true),
        exportable: privacy_node
            .and_then(|n| n.get("exportable"))
            .and_then(Yaml::as_bool)
            .unwrap_or(true),
        sensitivity: privacy_node
            .and_then(|n| n.get("sensitivity"))
            .and_then(Yaml::as_string)
            .and_then(|s| parse_sensitivity(&s))
            .unwrap_or(SensitivityClass::Normal),
    };

    Ok(CanonicalMemory {
        id,
        owner,
        kind,
        predicate,
        status,
        confidence,
        subject,
        value,
        statement: doc.section(SECTION_FACT).unwrap_or_default().to_string(),
        evidence_summary: doc
            .section(SECTION_EVIDENCE)
            .unwrap_or_default()
            .to_string(),
        source,
        temporal,
        retrieval,
        evidence,
        privacy,
        temporal_scope,
        supersedes: doc
            .section_list(SECTION_SUPERSEDES)
            .into_iter()
            .map(MemoryId::new)
            .collect(),
        superseded_by: doc
            .front
            .get("superseded_by")
            .and_then(Yaml::as_string)
            .map(MemoryId::new),
        qualifier: doc.front.get("qualifier").and_then(Yaml::as_string),
    })
}

fn value_to_yaml(value: &MemoryValue) -> Yaml {
    match value {
        MemoryValue::Text(t) => yaml::map(vec![
            ("type", Some("text".into())),
            ("value", Some(t.clone().into())),
        ]),
        MemoryValue::Bool(b) => yaml::map(vec![
            ("type", Some("bool".into())),
            ("value", Some((*b).into())),
        ]),
        MemoryValue::Number(n) => yaml::map(vec![
            ("type", Some("number".into())),
            ("value", Some(Yaml::Float(*n))),
        ]),
        MemoryValue::List(items) => yaml::map(vec![
            ("type", Some("list".into())),
            ("value", Some(yaml::seq_of_strings(items.clone()))),
        ]),
    }
}

fn yaml_to_value(node: &Yaml) -> Result<MemoryValue, String> {
    let kind = node
        .get("type")
        .and_then(Yaml::as_string)
        .ok_or_else(|| "value block missing `type`".to_string())?;
    let raw = node.get("value").unwrap_or(&Yaml::Null);
    match kind.as_str() {
        "text" => Ok(MemoryValue::Text(raw.as_string().unwrap_or_default())),
        "bool" => Ok(MemoryValue::Bool(raw.as_bool().unwrap_or(false))),
        "number" => Ok(MemoryValue::Number(raw.as_f64().unwrap_or(0.0))),
        "list" => Ok(MemoryValue::List(raw.as_string_list())),
        other => Err(format!("unknown value type `{other}`")),
    }
}

fn timestamp(value: DateTime<Utc>) -> Yaml {
    Yaml::Str(value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

fn optional_timestamp(value: Option<DateTime<Utc>>) -> Yaml {
    value.map(timestamp).unwrap_or(Yaml::Null)
}

fn required_time(node: &Yaml, key: &str, path: &str) -> Result<DateTime<Utc>, MemoryError> {
    optional_time(node, key).ok_or_else(|| MemoryError::MalformedRecord {
        path: path.to_string(),
        message: format!("missing or unparsable `temporal.{key}`"),
    })
}

fn optional_time(node: &Yaml, key: &str) -> Option<DateTime<Utc>> {
    node.get(key)
        .and_then(Yaml::as_string)
        .and_then(|raw| DateTime::parse_from_rfc3339(&raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

fn parse_turn_id(raw: &str) -> Option<TurnId> {
    raw.strip_prefix("turn_")
        .unwrap_or(raw)
        .parse::<u64>()
        .ok()
        .map(TurnId)
}

fn status_label(status: MemoryStatus) -> &'static str {
    match status {
        MemoryStatus::Active => "active",
        MemoryStatus::Staged => "staged",
        MemoryStatus::Superseded => "superseded",
        MemoryStatus::Expired => "expired",
        MemoryStatus::Deleted => "deleted",
    }
}

fn parse_status(raw: &str) -> Option<MemoryStatus> {
    Some(match raw {
        "active" => MemoryStatus::Active,
        "staged" => MemoryStatus::Staged,
        "superseded" => MemoryStatus::Superseded,
        "expired" => MemoryStatus::Expired,
        "deleted" => MemoryStatus::Deleted,
        _ => return None,
    })
}

fn temporal_scope_label(scope: TemporalScope) -> &'static str {
    match scope {
        TemporalScope::Persistent => "persistent",
        TemporalScope::RecentHistory => "recent_history",
        TemporalScope::Momentary => "momentary",
        TemporalScope::Scheduled => "scheduled",
    }
}

fn parse_temporal_scope(raw: &str) -> Option<TemporalScope> {
    Some(match raw {
        "persistent" => TemporalScope::Persistent,
        "recent_history" => TemporalScope::RecentHistory,
        "momentary" => TemporalScope::Momentary,
        "scheduled" => TemporalScope::Scheduled,
        _ => return None,
    })
}

fn sensitivity_label(class: SensitivityClass) -> &'static str {
    match class {
        SensitivityClass::Normal => "normal",
        SensitivityClass::Sensitive => "sensitive",
        SensitivityClass::Restricted => "restricted",
    }
}

fn parse_sensitivity(raw: &str) -> Option<SensitivityClass> {
    Some(match raw {
        "normal" => SensitivityClass::Normal,
        "sensitive" => SensitivityClass::Sensitive,
        "restricted" => SensitivityClass::Restricted,
        _ => return None,
    })
}

fn parse_kind(raw: &str) -> Option<MemoryKind> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Explicitness, ids::SessionId};

    fn sample() -> CanonicalMemory {
        let now = DateTime::parse_from_rfc3339("2026-07-26T09:12:14Z")
            .unwrap()
            .with_timezone(&Utc);
        CanonicalMemory {
            id: MemoryId::new("mem_01K4D8P3"),
            owner: UserId::new("usr_72ab"),
            kind: MemoryKind::Preference,
            predicate: CanonicalPredicate::new("dietary_identity"),
            status: MemoryStatus::Active,
            confidence: 1.0,
            subject: EntityRef::user(),
            value: MemoryValue::Text("pescatarian".into()),
            statement: "The user is pescatarian.".into(),
            evidence_summary: "Explicitly stated by the user.".into(),
            source: MemorySource::from_explicitness(
                Explicitness::ExplicitStatement,
                SessionId::new("ses_01K4"),
                TurnId(17),
            ),
            temporal: TemporalMetadata::created_at(now),
            retrieval: RetrievalMetadata {
                subject: "user".into(),
                tags: vec!["food".into(), "diet".into(), "pescatarian".into()],
                aliases: vec!["does not eat meat".into(), "eats fish".into()],
                entities: vec![],
                location: None,
            },
            evidence: EvidenceCounters::first(),
            privacy: PrivacyMetadata::default(),
            temporal_scope: TemporalScope::Persistent,
            supersedes: vec![MemoryId::new("mem_01JVEGETARIAN")],
            superseded_by: None,
            qualifier: None,
        }
    }

    #[test]
    fn round_trips_through_markdown() {
        let original = sample();
        let markdown = to_document(&original).to_markdown();
        let reparsed = OkfDocument::parse(&markdown, "sample.md").unwrap();
        let recovered = from_document(&reparsed, "sample.md").unwrap();
        assert_eq!(original, recovered);
    }

    #[test]
    fn the_rendered_file_is_human_readable() {
        let markdown = to_document(&sample()).to_markdown();
        assert!(markdown.starts_with("---\nokf: memory/v1\n"));
        assert!(markdown.contains("\n# Fact\nThe user is pescatarian.\n"));
        assert!(markdown.contains("turn_id: turn_17"));
        assert!(markdown.contains("created_at: 2026-07-26T09:12:14Z"));
        assert!(markdown.contains("- mem_01JVEGETARIAN"));
    }

    #[test]
    fn an_unknown_format_version_is_refused() {
        let mut doc = to_document(&sample());
        if let Yaml::Map(entries) = &mut doc.front {
            entries[0].1 = Yaml::Str("memory/v99".into());
        }
        let err = from_document(&doc, "x.md").unwrap_err();
        assert!(err.to_string().contains("unsupported OKF version"));
    }

    #[test]
    fn hand_edited_records_missing_optional_blocks_still_parse() {
        let minimal = r#"---
okf: memory/v1
id: mem_hand
owner: usr_72ab
kind: preference
predicate: coffee_order
status: active
confidence: 0.9
value:
  type: text
  value: flat white
temporal:
  created_at: 2026-07-26T09:12:14Z
---
# Fact
The user drinks flat whites.
"#;
        let doc = OkfDocument::parse(minimal, "minimal.md").unwrap();
        let memory = from_document(&doc, "minimal.md").unwrap();
        assert_eq!(memory.statement, "The user drinks flat whites.");
        assert_eq!(memory.evidence.count, 1);
        assert_eq!(memory.temporal.updated_at, memory.temporal.created_at);
        assert!(memory.privacy.deletable);
    }

    #[test]
    fn statements_containing_colons_survive_the_round_trip() {
        let mut memory = sample();
        memory.statement = "The user said: quiet places only.".into();
        memory.qualifier = Some("with family".into());
        let markdown = to_document(&memory).to_markdown();
        let recovered =
            from_document(&OkfDocument::parse(&markdown, "x.md").unwrap(), "x.md").unwrap();
        assert_eq!(recovered.statement, memory.statement);
        assert_eq!(recovered.qualifier.as_deref(), Some("with family"));
    }
}
