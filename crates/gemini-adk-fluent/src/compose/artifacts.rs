//! A — Artifact composition.
//!
//! Compose artifact schemas and transforms with `+`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// An artifact schema describing expected artifact structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSchema {
    /// Artifact name/key.
    pub name: String,
    /// MIME type.
    pub mime_type: String,
    /// Description of what this artifact contains.
    pub description: String,
}

/// An artifact transform — a pipeline step that produces or consumes artifacts.
#[derive(Debug, Clone)]
pub struct ArtifactTransform {
    /// Artifacts consumed (input).
    pub inputs: Vec<ArtifactSchema>,
    /// Artifacts produced (output).
    pub outputs: Vec<ArtifactSchema>,
}

impl ArtifactTransform {
    /// Create a transform that only produces artifacts.
    pub fn produces(schemas: Vec<ArtifactSchema>) -> Self {
        Self {
            inputs: Vec::new(),
            outputs: schemas,
        }
    }

    /// Create a transform that only consumes artifacts.
    pub fn consumes(schemas: Vec<ArtifactSchema>) -> Self {
        Self {
            inputs: schemas,
            outputs: Vec::new(),
        }
    }

    /// Number of input + output schemas.
    pub fn len(&self) -> usize {
        self.inputs.len() + self.outputs.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty()
    }
}

/// An artifact composite — multiple transforms composed together.
#[derive(Debug, Clone)]
pub struct ArtifactComposite {
    /// The list of artifact transforms in this composite.
    pub transforms: Vec<ArtifactTransform>,
}

impl ArtifactComposite {
    /// Create from a single transform.
    pub fn from_transform(transform: ArtifactTransform) -> Self {
        Self {
            transforms: vec![transform],
        }
    }

    /// All input schemas across all transforms.
    pub fn all_inputs(&self) -> Vec<&ArtifactSchema> {
        self.transforms.iter().flat_map(|t| &t.inputs).collect()
    }

    /// All output schemas across all transforms.
    pub fn all_outputs(&self) -> Vec<&ArtifactSchema> {
        self.transforms.iter().flat_map(|t| &t.outputs).collect()
    }

    /// Total number of transforms.
    pub fn len(&self) -> usize {
        self.transforms.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.transforms.is_empty()
    }
}

/// Compose two artifact composites with `+`.
impl std::ops::Add for ArtifactComposite {
    type Output = ArtifactComposite;

    fn add(mut self, rhs: ArtifactComposite) -> Self::Output {
        self.transforms.extend(rhs.transforms);
        self
    }
}

/// The `A` namespace — static factory methods for artifact composition.
pub struct A;

impl A {
    /// Declare an artifact that this agent produces.
    pub fn output(
        name: impl Into<String>,
        mime_type: impl Into<String>,
        description: impl Into<String>,
    ) -> ArtifactComposite {
        ArtifactComposite::from_transform(ArtifactTransform::produces(vec![ArtifactSchema {
            name: name.into(),
            mime_type: mime_type.into(),
            description: description.into(),
        }]))
    }

    /// Declare an artifact that this agent consumes.
    pub fn input(
        name: impl Into<String>,
        mime_type: impl Into<String>,
        description: impl Into<String>,
    ) -> ArtifactComposite {
        ArtifactComposite::from_transform(ArtifactTransform::consumes(vec![ArtifactSchema {
            name: name.into(),
            mime_type: mime_type.into(),
            description: description.into(),
        }]))
    }

    /// Declare a JSON artifact output.
    pub fn json_output(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> ArtifactComposite {
        Self::output(name, "application/json", description)
    }

    /// Declare a JSON artifact input.
    pub fn json_input(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> ArtifactComposite {
        Self::input(name, "application/json", description)
    }

    /// Declare a text artifact output.
    pub fn text_output(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> ArtifactComposite {
        Self::output(name, "text/plain", description)
    }

    /// Declare a text artifact input.
    pub fn text_input(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> ArtifactComposite {
        Self::input(name, "text/plain", description)
    }

    /// Publish an artifact with the given name and MIME type.
    pub fn publish(name: impl Into<String>, mime_type: impl Into<String>) -> ArtifactOp {
        ArtifactOp::Publish {
            name: name.into(),
            mime_type: mime_type.into(),
        }
    }

    /// Save an artifact to storage.
    pub fn save(name: impl Into<String>) -> ArtifactOp {
        ArtifactOp::Save { name: name.into() }
    }

    /// Load an artifact from storage.
    pub fn load(name: impl Into<String>) -> ArtifactOp {
        ArtifactOp::Load { name: name.into() }
    }

    /// List available artifacts.
    pub fn list() -> ArtifactOp {
        ArtifactOp::List
    }

    /// Delete an artifact.
    pub fn delete(name: impl Into<String>) -> ArtifactOp {
        ArtifactOp::Delete { name: name.into() }
    }

    /// Get a specific version of an artifact.
    pub fn version(name: impl Into<String>, version: u32) -> ArtifactOp {
        ArtifactOp::Version {
            name: name.into(),
            version,
        }
    }

    /// Convert an artifact to JSON format.
    pub fn as_json(name: impl Into<String>) -> ArtifactOp {
        ArtifactOp::AsJson { name: name.into() }
    }

    /// Convert an artifact to text format.
    pub fn as_text(name: impl Into<String>) -> ArtifactOp {
        ArtifactOp::AsText { name: name.into() }
    }

    /// Create an artifact from a JSON string.
    pub fn from_json(name: impl Into<String>, data: impl Into<String>) -> ArtifactOp {
        ArtifactOp::FromJson {
            name: name.into(),
            data: data.into(),
        }
    }

    /// Create an artifact from a text string.
    pub fn from_text(name: impl Into<String>, data: impl Into<String>) -> ArtifactOp {
        ArtifactOp::FromText {
            name: name.into(),
            data: data.into(),
        }
    }

    /// Conditional artifact operation — executes `inner` only when `predicate` returns true.
    pub fn when(
        predicate: impl Fn() -> bool + Send + Sync + 'static,
        inner: ArtifactOp,
    ) -> ArtifactOp {
        ArtifactOp::When {
            predicate: Arc::new(predicate),
            inner: Box::new(inner),
        }
    }
}

/// A runtime artifact operation.
///
/// These represent deferred operations on artifacts that can be composed
/// into pipelines using the `+` operator.
#[derive(Clone)]
pub enum ArtifactOp {
    /// Publish an artifact with a given MIME type.
    Publish {
        /// Artifact name.
        name: String,
        /// MIME type.
        mime_type: String,
    },
    /// Save an artifact to storage.
    Save {
        /// Artifact name.
        name: String,
    },
    /// Load an artifact from storage.
    Load {
        /// Artifact name.
        name: String,
    },
    /// List available artifacts.
    List,
    /// Delete an artifact.
    Delete {
        /// Artifact name.
        name: String,
    },
    /// Get a specific version of an artifact.
    Version {
        /// Artifact name.
        name: String,
        /// Version number.
        version: u32,
    },
    /// Convert an artifact to JSON format.
    AsJson {
        /// Artifact name.
        name: String,
    },
    /// Convert an artifact to text format.
    AsText {
        /// Artifact name.
        name: String,
    },
    /// Create an artifact from a JSON string.
    FromJson {
        /// Artifact name.
        name: String,
        /// JSON data.
        data: String,
    },
    /// Create an artifact from a text string.
    FromText {
        /// Artifact name.
        name: String,
        /// Text data.
        data: String,
    },
    /// Conditional operation — execute inner only when predicate is true.
    When {
        /// Predicate function.
        #[allow(clippy::type_complexity)]
        predicate: Arc<dyn Fn() -> bool + Send + Sync>,
        /// Inner operation to conditionally execute.
        inner: Box<ArtifactOp>,
    },
    /// A sequence of operations composed with `+`.
    Sequence(Vec<ArtifactOp>),
}

impl ArtifactOp {
    /// Returns the artifact name associated with this operation, if any.
    pub fn name(&self) -> Option<&str> {
        match self {
            ArtifactOp::Publish { name, .. }
            | ArtifactOp::Save { name }
            | ArtifactOp::Load { name }
            | ArtifactOp::Delete { name }
            | ArtifactOp::Version { name, .. }
            | ArtifactOp::AsJson { name }
            | ArtifactOp::AsText { name }
            | ArtifactOp::FromJson { name, .. }
            | ArtifactOp::FromText { name, .. } => Some(name),
            ArtifactOp::List => None,
            ArtifactOp::When { inner, .. } => inner.name(),
            ArtifactOp::Sequence(_) => None,
        }
    }

    /// Returns true if this operation should execute (always true unless `When`).
    pub fn should_execute(&self) -> bool {
        match self {
            ArtifactOp::When { predicate, .. } => predicate(),
            _ => true,
        }
    }

    /// Flatten this operation into a list of leaf operations.
    pub fn flatten(&self) -> Vec<&ArtifactOp> {
        match self {
            ArtifactOp::Sequence(ops) => ops.iter().flat_map(|op| op.flatten()).collect(),
            other => vec![other],
        }
    }
}

impl std::fmt::Debug for ArtifactOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtifactOp::Publish { name, mime_type } => f
                .debug_struct("Publish")
                .field("name", name)
                .field("mime_type", mime_type)
                .finish(),
            ArtifactOp::Save { name } => f.debug_struct("Save").field("name", name).finish(),
            ArtifactOp::Load { name } => f.debug_struct("Load").field("name", name).finish(),
            ArtifactOp::List => write!(f, "List"),
            ArtifactOp::Delete { name } => f.debug_struct("Delete").field("name", name).finish(),
            ArtifactOp::Version { name, version } => f
                .debug_struct("Version")
                .field("name", name)
                .field("version", version)
                .finish(),
            ArtifactOp::AsJson { name } => f.debug_struct("AsJson").field("name", name).finish(),
            ArtifactOp::AsText { name } => f.debug_struct("AsText").field("name", name).finish(),
            ArtifactOp::FromJson { name, .. } => {
                f.debug_struct("FromJson").field("name", name).finish()
            }
            ArtifactOp::FromText { name, .. } => {
                f.debug_struct("FromText").field("name", name).finish()
            }
            ArtifactOp::When { inner, .. } => f.debug_struct("When").field("inner", inner).finish(),
            ArtifactOp::Sequence(ops) => f.debug_struct("Sequence").field("ops", ops).finish(),
        }
    }
}

/// Compose two artifact operations with `+`.
impl std::ops::Add for ArtifactOp {
    type Output = ArtifactOp;

    fn add(self, rhs: ArtifactOp) -> Self::Output {
        match self {
            ArtifactOp::Sequence(mut ops) => {
                match rhs {
                    ArtifactOp::Sequence(rhs_ops) => ops.extend(rhs_ops),
                    other => ops.push(other),
                }
                ArtifactOp::Sequence(ops)
            }
            other => match rhs {
                ArtifactOp::Sequence(mut rhs_ops) => {
                    rhs_ops.insert(0, other);
                    ArtifactOp::Sequence(rhs_ops)
                }
                rhs_other => ArtifactOp::Sequence(vec![other, rhs_other]),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_schema() {
        let schema = ArtifactSchema {
            name: "report".into(),
            mime_type: "application/json".into(),
            description: "Analysis report".into(),
        };
        assert_eq!(schema.name, "report");
    }

    #[test]
    fn artifact_transform_produces() {
        let t = ArtifactTransform::produces(vec![ArtifactSchema {
            name: "output".into(),
            mime_type: "text/plain".into(),
            description: "Result".into(),
        }]);
        assert_eq!(t.outputs.len(), 1);
        assert!(t.inputs.is_empty());
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn artifact_transform_consumes() {
        let t = ArtifactTransform::consumes(vec![ArtifactSchema {
            name: "input".into(),
            mime_type: "text/plain".into(),
            description: "Source".into(),
        }]);
        assert!(t.outputs.is_empty());
        assert_eq!(t.inputs.len(), 1);
    }

    #[test]
    fn a_json_output() {
        let comp = A::json_output("report", "Analysis results");
        assert_eq!(comp.len(), 1);
        let outputs = comp.all_outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].mime_type, "application/json");
    }

    #[test]
    fn a_text_input() {
        let comp = A::text_input("source", "Source document");
        let inputs = comp.all_inputs();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].mime_type, "text/plain");
    }

    #[test]
    fn compose_with_add() {
        let comp = A::json_output("report", "Report")
            + A::text_input("source", "Source")
            + A::json_output("summary", "Summary");
        assert_eq!(comp.len(), 3);
        assert_eq!(comp.all_inputs().len(), 1);
        assert_eq!(comp.all_outputs().len(), 2);
    }

    #[test]
    fn empty_composite() {
        let comp = ArtifactComposite { transforms: vec![] };
        assert!(comp.is_empty());
        assert_eq!(comp.len(), 0);
    }

    #[test]
    fn publish_op() {
        let op = A::publish("report", "application/json");
        assert_eq!(op.name(), Some("report"));
        assert!(op.should_execute());
    }

    #[test]
    fn save_and_load_ops() {
        let save = A::save("report");
        let load = A::load("report");
        assert_eq!(save.name(), Some("report"));
        assert_eq!(load.name(), Some("report"));
    }

    #[test]
    fn list_op() {
        let op = A::list();
        assert_eq!(op.name(), None);
        assert!(op.should_execute());
    }

    #[test]
    fn delete_op() {
        let op = A::delete("old_report");
        assert_eq!(op.name(), Some("old_report"));
    }

    #[test]
    fn version_op() {
        let op = A::version("report", 3);
        assert_eq!(op.name(), Some("report"));
        if let ArtifactOp::Version { version, .. } = &op {
            assert_eq!(*version, 3);
        } else {
            panic!("Expected Version variant");
        }
    }

    #[test]
    fn as_json_and_as_text() {
        let json_op = A::as_json("data");
        let text_op = A::as_text("data");
        assert_eq!(json_op.name(), Some("data"));
        assert_eq!(text_op.name(), Some("data"));
    }

    #[test]
    fn from_json_and_from_text() {
        let json_op = A::from_json("config", r#"{"key": "value"}"#);
        let text_op = A::from_text("note", "hello world");
        assert_eq!(json_op.name(), Some("config"));
        assert_eq!(text_op.name(), Some("note"));
    }

    #[test]
    fn when_op_true() {
        let op = A::when(|| true, A::save("report"));
        assert!(op.should_execute());
        assert_eq!(op.name(), Some("report"));
    }

    #[test]
    fn when_op_false() {
        let op = A::when(|| false, A::save("report"));
        assert!(!op.should_execute());
    }

    #[test]
    fn compose_ops_with_add() {
        let pipeline = A::load("source") + A::as_json("source") + A::save("output");
        let ops = pipeline.flatten();
        assert_eq!(ops.len(), 3);
    }

    #[test]
    fn op_debug_format() {
        let op = A::publish("report", "application/json");
        let debug = format!("{:?}", op);
        assert!(debug.contains("Publish"));
        assert!(debug.contains("report"));
    }
}
