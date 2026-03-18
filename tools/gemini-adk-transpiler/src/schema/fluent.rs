use serde::{Deserialize, Serialize};

use super::common::SourceInfo;

/// Schema for the adk-fluent Python package.
/// Captures builder methods, factory functions, operators, and workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluentSchema {
    /// Source information
    pub source: SourceInfo,
    /// Agent builder methods
    pub builder_methods: Vec<FluentMethodDef>,
    /// Factory functions (C::window, P::role, etc.)
    pub factories: Vec<FluentFactoryDef>,
    /// Operator overloads (+, |, >>)
    pub operators: Vec<FluentOperatorDef>,
    /// Workflow builders (Pipeline, FanOut, Loop)
    pub workflows: Vec<FluentWorkflowDef>,
}

/// A builder method extracted from adk-fluent's agent.py.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluentMethodDef {
    /// Method name (e.g., "model", "instruction", "temperature")
    pub name: String,
    /// Python parameter type (e.g., "str", "float", "BaseLlm | str")
    pub param_type: String,
    /// Mapped Rust type
    pub rust_type: String,
    /// Whether the method returns Self (builder chaining)
    pub returns_self: bool,
    /// Docstring description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A factory function from C/P/S/M/T/A modules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluentFactoryDef {
    /// Module namespace (C, P, S, M, T, A)
    pub module: FluentModule,
    /// Function name (e.g., "window", "role", "pick")
    pub name: String,
    /// Parameters with types
    pub params: Vec<FluentParam>,
    /// Return type description
    pub return_type: String,
    /// Docstring description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A parameter in a factory function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluentParam {
    /// Parameter name
    pub name: String,
    /// Python type annotation
    pub py_type: String,
    /// Default value (if any)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// Which fluent composition module a factory belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FluentModule {
    /// Context engineering
    Context,
    /// Prompt composition
    Prompt,
    /// State transforms
    State,
    /// Middleware
    Middleware,
    /// Tools
    Tools,
    /// Artifacts
    Artifacts,
}

impl FluentModule {
    /// Returns the Rust module name.
    pub fn rust_module(&self) -> &'static str {
        match self {
            Self::Context => "c",
            Self::Prompt => "p",
            Self::State => "s",
            Self::Middleware => "m",
            Self::Tools => "t",
            Self::Artifacts => "a",
        }
    }
}

/// An operator overload from adk-fluent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluentOperatorDef {
    /// Left-hand side type
    pub lhs: String,
    /// Right-hand side type
    pub rhs: String,
    /// Output type
    pub output: String,
    /// Python operator ("+", "|", ">>", "*", "/")
    pub operator: String,
}

/// A workflow builder definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FluentWorkflowDef {
    /// Workflow name (Pipeline, FanOut, Loop)
    pub name: String,
    /// Builder methods
    pub methods: Vec<FluentMethodDef>,
    /// Docstring description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
