//! A — Artifact composition.
//!
//! Compose artifact schemas and transforms with `+`.

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
}
