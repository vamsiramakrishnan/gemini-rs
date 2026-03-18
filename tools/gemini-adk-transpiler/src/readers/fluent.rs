//! Python source reader for adk-fluent.
//!
//! Reads .py and .pyi files from the adk-fluent source tree and extracts:
//! - Agent builder methods from agent.py / agent.pyi
//! - Factory functions from _context.py, _prompt.py, _transforms.py, _tools.py, _artifacts.py
//! - Operator overloads
//! - Workflow builders from workflow.py

use std::path::Path;

use regex::Regex;

use crate::schema::fluent::*;
use crate::schema::SourceInfo;

use super::typescript as ts;

/// Read the adk-fluent Python source directory and extract a FluentSchema.
pub fn read_fluent_source(source_dir: &Path) -> Result<FluentSchema, String> {
    if !source_dir.exists() {
        return Err(format!(
            "Source directory does not exist: {}",
            source_dir.display()
        ));
    }

    let mut builder_methods = Vec::new();
    let mut factories = Vec::new();
    let mut operators = Vec::new();
    let mut workflows = Vec::new();

    // Read agent builder methods
    let agent_py = source_dir.join("agent.py");
    let agent_pyi = source_dir.join("agent.pyi");
    if agent_pyi.exists() {
        let content = read_file(&agent_pyi)?;
        builder_methods.extend(extract_builder_methods(&content));
    } else if agent_py.exists() {
        let content = read_file(&agent_py)?;
        builder_methods.extend(extract_builder_methods(&content));
    }

    // Read factory functions from composition modules
    let module_files = [
        ("_context.py", FluentModule::Context),
        ("_prompt.py", FluentModule::Prompt),
        ("_transforms.py", FluentModule::State),
        ("_middleware.py", FluentModule::Middleware),
        ("_tools.py", FluentModule::Tools),
        ("_artifacts.py", FluentModule::Artifacts),
    ];

    for (filename, module) in &module_files {
        let path = source_dir.join(filename);
        if path.exists() {
            let content = read_file(&path)?;
            factories.extend(extract_factories(&content, module.clone()));
        }
    }

    // Read workflow builders
    let workflow_py = source_dir.join("workflow.py");
    let workflow_pyi = source_dir.join("workflow.pyi");
    if workflow_pyi.exists() {
        let content = read_file(&workflow_pyi)?;
        workflows.extend(extract_workflows(&content));
    } else if workflow_py.exists() {
        let content = read_file(&workflow_py)?;
        workflows.extend(extract_workflows(&content));
    }

    // Extract operator overloads from _base.py and module files
    let base_py = source_dir.join("_base.py");
    if base_py.exists() {
        let content = read_file(&base_py)?;
        operators.extend(extract_operators(&content));
    }

    // Also check _primitives.py for operator definitions
    let primitives_py = source_dir.join("_primitives.py");
    if primitives_py.exists() {
        let content = read_file(&primitives_py)?;
        operators.extend(extract_operators(&content));
    }

    let source = SourceInfo {
        framework: "adk-fluent".to_string(),
        source_dir: source_dir.display().to_string(),
        extracted_at: ts::chrono_like_now(),
    };

    Ok(FluentSchema {
        source,
        builder_methods,
        factories,
        operators,
        workflows,
    })
}

fn read_file(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {}", path.display(), e))
}

/// Extract builder methods from agent.py or agent.pyi.
///
/// Looks for patterns like:
///   def model(self, value: str | BaseLlm) -> Self:
///   def temperature(self, value: float) -> Self:
fn extract_builder_methods(content: &str) -> Vec<FluentMethodDef> {
    let mut methods = Vec::new();

    // Pattern: def method_name(self, param: type) -> Self:
    let re = Regex::new(
        r#"def\s+(\w+)\s*\(\s*self\s*,\s*(\w+)\s*:\s*([^)]+?)\s*\)\s*->\s*(?:Self|"[^"]*")"#,
    )
    .unwrap();

    // Also capture methods with no params (just self)
    let re_no_param =
        Regex::new(r#"def\s+(\w+)\s*\(\s*self\s*\)\s*->\s*(?:Self|"[^"]*")"#).unwrap();

    let mut current_docstring: Option<String> = None;
    let lines: Vec<&str> = content.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Capture docstring from next line(s)
        if trimmed.starts_with("\"\"\"") && trimmed.ends_with("\"\"\"") && trimmed.len() > 6 {
            current_docstring = Some(trimmed.trim_matches('"').trim().to_string());
            continue;
        }

        if let Some(caps) = re.captures(trimmed) {
            let name = caps[1].to_string();
            let param_type = caps[3].trim().to_string();

            // Skip private methods and dunder methods
            if name.starts_with('_') {
                current_docstring = None;
                continue;
            }

            let rust_type = python_type_to_rust(&param_type);

            // Look for docstring in the next line
            let desc = current_docstring.take().or_else(|| {
                if i + 1 < lines.len() {
                    let next = lines[i + 1].trim();
                    if next.starts_with("\"\"\"") {
                        Some(next.trim_matches('"').trim().to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

            methods.push(FluentMethodDef {
                name,
                param_type: param_type.clone(),
                rust_type,
                returns_self: true,
                description: desc,
            });
        } else if let Some(caps) = re_no_param.captures(trimmed) {
            let name = caps[1].to_string();
            if !name.starts_with('_') {
                methods.push(FluentMethodDef {
                    name,
                    param_type: String::new(),
                    rust_type: String::new(),
                    returns_self: true,
                    description: current_docstring.take(),
                });
            }
        } else {
            // Reset docstring if we see a non-def line that isn't a docstring
            if !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with("\"\"\"") {
                current_docstring = None;
            }
        }
    }

    methods
}

/// Extract factory functions from composition module files.
///
/// Looks for patterns like:
///   def window(n: int = 5) -> CTransform:
///   def role(text: str) -> PromptSection:
///
/// Also looks for @staticmethod and @classmethod decorators.
fn extract_factories(content: &str, module: FluentModule) -> Vec<FluentFactoryDef> {
    let mut factories = Vec::new();

    // Pattern: def func_name(param: type = default, ...) -> ReturnType:
    let re = Regex::new(r#"def\s+(\w+)\s*\(([^)]*)\)\s*->\s*(?:"([^"]+)"|(\w[\w\[\], |]*))\s*:"#)
        .unwrap();

    let param_re = Regex::new(r#"(\w+)\s*:\s*([^=,]+?)(?:\s*=\s*([^,]+))?\s*(?:,|$)"#).unwrap();

    let lines: Vec<&str> = content.lines().collect();
    let mut current_docstring: Option<String> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Capture inline docstring
        if trimmed.starts_with("\"\"\"") && trimmed.ends_with("\"\"\"") && trimmed.len() > 6 {
            current_docstring = Some(trimmed.trim_matches('"').trim().to_string());
            continue;
        }

        if let Some(caps) = re.captures(trimmed) {
            let name = caps[1].to_string();
            let params_str = &caps[2];
            let return_type = caps
                .get(3)
                .or_else(|| caps.get(4))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();

            // Skip private, dunder, and self-methods
            if name.starts_with('_') || params_str.trim_start().starts_with("self") {
                current_docstring = None;
                continue;
            }

            let mut params = Vec::new();
            for pcaps in param_re.captures_iter(params_str) {
                let pname = pcaps[1].to_string();
                let py_type = pcaps[2].trim().to_string();
                let default = pcaps.get(3).map(|m| m.as_str().trim().to_string());
                params.push(FluentParam {
                    name: pname,
                    py_type,
                    default,
                });
            }

            // Look for docstring
            let desc = current_docstring.take().or_else(|| {
                if i + 1 < lines.len() {
                    let next = lines[i + 1].trim();
                    if next.starts_with("\"\"\"") {
                        Some(next.trim_matches('"').trim().to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

            factories.push(FluentFactoryDef {
                module: module.clone(),
                name,
                params,
                return_type,
                description: desc,
            });
        } else if !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("\"\"\"")
            && !trimmed.starts_with('@')
        {
            current_docstring = None;
        }
    }

    factories
}

/// Extract workflow builder definitions from workflow.py.
fn extract_workflows(content: &str) -> Vec<FluentWorkflowDef> {
    let mut workflows = Vec::new();

    // Find class definitions
    let class_re = Regex::new(r#"class\s+(\w+)(?:\([^)]*\))?\s*:"#).unwrap();
    let method_re = Regex::new(
        r#"def\s+(\w+)\s*\(\s*self\s*(?:,\s*(\w+)\s*:\s*([^)]+?))?\s*\)\s*->\s*(?:Self|"[^"]*")"#,
    )
    .unwrap();

    let lines: Vec<&str> = content.lines().collect();
    let mut current_class: Option<String> = None;
    let mut current_methods: Vec<FluentMethodDef> = Vec::new();
    let mut current_doc: Option<String> = None;

    for line in &lines {
        let trimmed = line.trim();

        if let Some(caps) = class_re.captures(trimmed) {
            // Save previous class
            if let Some(ref name) = current_class {
                if !current_methods.is_empty() {
                    workflows.push(FluentWorkflowDef {
                        name: name.clone(),
                        methods: std::mem::take(&mut current_methods),
                        description: current_doc.take(),
                    });
                }
            }
            current_class = Some(caps[1].to_string());
            current_methods.clear();
            current_doc = None;
        }

        if current_class.is_some() {
            if let Some(caps) = method_re.captures(trimmed) {
                let name = caps[1].to_string();
                if !name.starts_with('_') {
                    let param_type = caps
                        .get(3)
                        .map(|m| m.as_str().trim().to_string())
                        .unwrap_or_default();
                    let rust_type = if param_type.is_empty() {
                        String::new()
                    } else {
                        python_type_to_rust(&param_type)
                    };
                    current_methods.push(FluentMethodDef {
                        name,
                        param_type,
                        rust_type,
                        returns_self: true,
                        description: None,
                    });
                }
            }

            // Capture class docstring
            if trimmed.starts_with("\"\"\"")
                && current_doc.is_none()
                && trimmed.ends_with("\"\"\"")
                && trimmed.len() > 6
            {
                current_doc = Some(trimmed.trim_matches('"').trim().to_string());
            }
        }
    }

    // Save last class
    if let Some(ref name) = current_class {
        if !current_methods.is_empty() {
            workflows.push(FluentWorkflowDef {
                name: name.clone(),
                methods: current_methods,
                description: current_doc,
            });
        }
    }

    workflows
}

/// Extract operator overloads from Python source.
fn extract_operators(content: &str) -> Vec<FluentOperatorDef> {
    let mut operators = Vec::new();

    // Look for __add__, __or__, __rshift__, __mul__, __truediv__ methods
    let re = Regex::new(
        r#"def\s+(__\w+__)\s*\(\s*self\s*,\s*(?:other|rhs)\s*:\s*(?:"([^"]+)"|(\w[\w\[\], |]*))\s*\)\s*->\s*(?:"([^"]+)"|(\w[\w\[\], |]*))"#,
    )
    .unwrap();

    // Track which class we're in
    let class_re = Regex::new(r#"class\s+(\w+)"#).unwrap();
    let mut current_class = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(caps) = class_re.captures(trimmed) {
            current_class = caps[1].to_string();
        }

        if let Some(caps) = re.captures(trimmed) {
            let method = &caps[1];
            let rhs = caps
                .get(2)
                .or_else(|| caps.get(3))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let output = caps
                .get(4)
                .or_else(|| caps.get(5))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();

            let operator = match method {
                "__add__" => "+",
                "__or__" | "__bitor__" => "|",
                "__rshift__" | "__shr__" => ">>",
                "__mul__" => "*",
                "__truediv__" | "__div__" => "/",
                _ => continue,
            };

            operators.push(FluentOperatorDef {
                lhs: current_class.clone(),
                rhs,
                output,
                operator: operator.to_string(),
            });
        }
    }

    operators
}

/// Convert a Python type annotation to a Rust type.
fn python_type_to_rust(py_type: &str) -> String {
    let trimmed = py_type.trim();

    // Handle union types (A | B)
    if trimmed.contains('|') {
        let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
        if parts.contains(&"None") {
            let non_none: Vec<&str> = parts.into_iter().filter(|&p| p != "None").collect();
            if non_none.len() == 1 {
                return format!("Option<{}>", python_type_to_rust(non_none[0]));
            }
        }
        // Multi-type union -> enum or Value
        return "serde_json::Value".to_string();
    }

    match trimmed {
        "str" => "String".to_string(),
        "int" => "i64".to_string(),
        "float" => "f64".to_string(),
        "bool" => "bool".to_string(),
        "None" => "()".to_string(),
        "Any" => "serde_json::Value".to_string(),
        "dict" | "Dict" => "HashMap<String, serde_json::Value>".to_string(),
        _ if trimmed.starts_with("list[") || trimmed.starts_with("List[") => {
            let inner = &trimmed[5..trimmed.len() - 1];
            format!("Vec<{}>", python_type_to_rust(inner))
        }
        _ if trimmed.starts_with("Optional[") => {
            let inner = &trimmed[9..trimmed.len() - 1];
            format!("Option<{}>", python_type_to_rust(inner))
        }
        _ if trimmed.starts_with("dict[") || trimmed.starts_with("Dict[") => {
            "HashMap<String, serde_json::Value>".to_string()
        }
        _ if trimmed.starts_with("Callable") => "Box<dyn Fn>".to_string(),
        _ => trimmed.to_string(), // Keep as-is for custom types
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_type_conversions() {
        assert_eq!(python_type_to_rust("str"), "String");
        assert_eq!(python_type_to_rust("int"), "i64");
        assert_eq!(python_type_to_rust("float"), "f64");
        assert_eq!(python_type_to_rust("bool"), "bool");
        assert_eq!(python_type_to_rust("list[str]"), "Vec<String>");
        assert_eq!(python_type_to_rust("Optional[int]"), "Option<i64>");
        assert_eq!(python_type_to_rust("str | None"), "Option<String>");
    }

    #[test]
    fn extract_builder_methods_basic() {
        let content = r#"
class Agent:
    def model(self, value: str) -> Self:
        """Set the model."""
        pass

    def temperature(self, value: float) -> Self:
        """Set temperature."""
        pass

    def _private(self, x: int) -> Self:
        pass
"#;
        let methods = extract_builder_methods(content);
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0].name, "model");
        assert_eq!(methods[0].rust_type, "String");
        assert_eq!(methods[1].name, "temperature");
        assert_eq!(methods[1].rust_type, "f64");
    }

    #[test]
    fn extract_factories_basic() {
        let content = r#"
def window(n: int = 5) -> CTransform:
    """Keep last n messages."""
    pass

def user_only() -> CTransform:
    pass

def _internal() -> CTransform:
    pass
"#;
        let factories = extract_factories(content, FluentModule::Context);
        assert_eq!(factories.len(), 2); // window + user_only (no params but has return type)
        assert_eq!(factories[0].name, "window");
        assert_eq!(factories[0].module, FluentModule::Context);
        assert_eq!(factories[0].params.len(), 1);
        assert_eq!(factories[0].params[0].default, Some("5".to_string()));
        assert_eq!(factories[1].name, "user_only");
    }

    #[test]
    fn extract_operators_basic() {
        let content = r#"
class CTransform:
    def __add__(self, other: "CTransform") -> "CTransform":
        pass

class PTransform:
    def __add__(self, other: "PTransform") -> "PTransform":
        pass
"#;
        let ops = extract_operators(content);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].lhs, "CTransform");
        assert_eq!(ops[0].operator, "+");
        assert_eq!(ops[1].lhs, "PTransform");
    }

    #[test]
    fn extract_workflows_basic() {
        let content = r#"
class Pipeline:
    """Sequential execution."""
    def step(self, agent: Agent) -> Self:
        pass

    def build(self) -> Self:
        pass
"#;
        let workflows = extract_workflows(content);
        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].name, "Pipeline");
        assert_eq!(workflows[0].methods.len(), 2);
    }

    #[test]
    fn fluent_module_rust_names() {
        assert_eq!(FluentModule::Context.rust_module(), "c");
        assert_eq!(FluentModule::Prompt.rust_module(), "p");
        assert_eq!(FluentModule::State.rust_module(), "s");
        assert_eq!(FluentModule::Middleware.rust_module(), "m");
        assert_eq!(FluentModule::Tools.rust_module(), "t");
        assert_eq!(FluentModule::Artifacts.rust_module(), "a");
    }
}
