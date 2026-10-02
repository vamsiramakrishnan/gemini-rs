//! Reusable task definitions and model-free task journeys.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gemini_adk_rs::flow::{Enforcement, Flow};
use gemini_adk_rs::state::State;
use gemini_adk_rs::tasks::{
    CompiledSkill, TaskEffect, TaskEffects, TaskPattern, TaskServices, TaskServicesFactory,
    TaskTool, TaskToolEffect, TaskWatcher,
};
use gemini_adk_rs::tool::{ContextTool, ToolContext, ToolDispatcher, ToolFunction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{SessionSpec, SpecResources, SpecTest, StateFieldSpec, StateType, ToolSpec};

/// One skill-local operation. Its effect policy is mandatory even for mocks.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillToolSpec {
    /// Declaration and external or mock binding.
    #[serde(flatten)]
    pub tool: ToolSpec,
    /// Reads can run immediately; commits require a trusted approval.
    pub effect: TaskToolEffect,
}

/// A versioned capability installed in one speaking session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillSpec {
    /// Unique installed capability name.
    pub name: String,
    /// Version pinned by each activation.
    pub version: String,
    /// Short routing description, available before activation.
    #[serde(default)]
    pub description: String,
    /// Instructions projected when this capability is foreground.
    #[serde(default)]
    pub instruction: String,
    /// Typed activation fields. Fields without defaults are required.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, StateFieldSpec>,
    /// Fields exported from accepted task state on completion.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, StateFieldSpec>,
    /// Task-private state and defaults.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state: BTreeMap<String, StateFieldSpec>,
    /// Operations, using skill-local names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<SkillToolSpec>,
    /// Structured extraction from this activation's owned transcript.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extract: Vec<super::ExtractSpec>,
    /// Derived task state, evaluated in dependency order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub computed: Vec<super::ComputedSpec>,
    /// Reactions to accepted task-state changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<super::WatchSpec>,
    /// Patterns over this activation's turns or clock.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<super::PatternSpec>,
    /// Existing contextual memory projected into this task's state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<super::MemorySpec>,
    /// Optional lower-level governed flow; mutually exclusive with conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<Flow>,
    /// Optional stages, digressions, repair and policies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<crate::conversation::ConversationSpec>,
    /// Existing model-free tests for the governed workflow.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<SpecTest>,
}

impl SkillSpec {
    pub(super) fn definition(&self) -> SessionSpec {
        let mut state = self.state.clone();
        state.extend(self.inputs.clone());
        for (name, field) in &self.outputs {
            state.entry(name.clone()).or_insert_with(|| field.clone());
        }
        SessionSpec {
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            instruction: self.instruction.clone(),
            state,
            tools: self.tools.iter().map(|t| t.tool.clone()).collect(),
            extract: self.extract.clone(),
            computed: self.computed.clone(),
            watch: self.watch.clone(),
            patterns: self.patterns.clone(),
            memory: self.memory.clone(),
            flow: self.flow.clone(),
            conversation: self.conversation.clone(),
            tests: self.tests.clone(),
            ..Default::default()
        }
    }

    /// Validate identity, field contracts, effect policies and governance.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        self.validate_with_http_support(true)
    }

    pub(super) fn validate_with_http_support(&self, http_support: bool) -> Result<(), Vec<String>> {
        let validation = self.validation(http_support);
        if validation.valid {
            Ok(())
        } else {
            Err(validation.errors)
        }
    }

    pub(super) fn validation(&self, http_support: bool) -> super::SpecValidation {
        let mut errors = Vec::new();
        if let Err(error) = gemini_adk_rs::tasks::validate_skill_identity(&self.name, &self.version)
        {
            errors.push(error.to_string());
        }
        let mut names = BTreeSet::new();
        for tool in &self.tools {
            if let Err(error) = task_parameters(tool) {
                errors.push(error);
            }
            if tool.tool.background || tool.tool.scheduling.is_some() {
                errors.push(format!(
                    "skill tool '{}' uses task-owned asynchronous delivery; background and scheduling overrides are unsupported",
                    tool.tool.name
                ));
            }
            if tool.tool.name.trim().is_empty() || !names.insert(tool.tool.name.as_str()) {
                errors.push(format!(
                    "skill '{}' has an empty or duplicate tool name '{}'",
                    self.name, tool.tool.name
                ));
            }
            if let Err(error) = (TaskTool {
                name: tool.tool.name.clone(),
                description: tool.tool.description.clone(),
                parameters: tool.tool.parameters.clone(),
                effect: tool.effect.clone(),
            })
            .validate_for_skill(&self.name)
            {
                errors.push(error.to_string());
            }
        }
        // Computed outputs own derived: storage; every other authored writer
        // must stay outside the task runtime's reserved namespaces.
        let mut writers = self.definition();
        writers.computed.clear();
        for key in writers
            .state_keys_written()
            .into_iter()
            .chain(self.computed.iter().map(|value| value.key.clone()))
        {
            if key.trim().is_empty()
                || ["task:", "flow:", "derived:"]
                    .iter()
                    .any(|prefix| key.starts_with(prefix))
            {
                errors.push(format!(
                    "skill '{}' writes empty or runtime-owned state key '{key}'",
                    self.name
                ));
            }
        }
        let mut extractor_names = BTreeSet::new();
        for extractor in &self.extract {
            if extractor.name.trim().is_empty() || !extractor_names.insert(&extractor.name) {
                errors.push(format!(
                    "skill '{}' has an empty or duplicate extractor name '{}'",
                    self.name, extractor.name
                ));
            }
            if extractor.window == 0 {
                errors.push(format!(
                    "task extractor '{}' needs a nonzero transcript window",
                    extractor.name
                ));
            }
            if extractor.trigger == super::TriggerSpec::OnPhaseChange {
                errors.push(format!("task extractor '{}' cannot use on_phase_change: task skills do not have phases", extractor.name));
            }
        }
        if self.memory.is_some() {
            for name in super::MEMORY_TOOL_NAMES {
                if names.contains(name) {
                    errors.push(format!(
                        "task memory owns tool '{name}'; do not redeclare it"
                    ));
                }
            }
        }
        for (scope, fields) in [
            ("input", &self.inputs),
            ("output", &self.outputs),
            ("state", &self.state),
        ] {
            for (name, field) in fields {
                if name.trim().is_empty()
                    || name.starts_with("flow:")
                    || name.starts_with("task:")
                    || name.starts_with("derived:")
                {
                    errors.push(format!(
                        "{scope} field '{name}' uses an empty or runtime-owned name"
                    ));
                }
                if scope != "state" && field.kind.is_none() {
                    errors.push(format!("{scope} field '{name}' needs a declared type"));
                }
                if let (Some(kind), Some(default)) = (field.kind, &field.default)
                    && !kind.matches(default)
                {
                    errors.push(format!(
                        "{scope} field '{name}' has a default of the wrong type"
                    ));
                }
                for other in [&self.state, &self.inputs, &self.outputs] {
                    if let Some(other) = other.get(name) {
                        if other.kind != field.kind {
                            errors.push(format!(
                                "field '{name}' has conflicting types across contracts"
                            ));
                        }
                        if matches!((&other.default, &field.default), (Some(a), Some(b)) if a != b)
                        {
                            errors.push(format!(
                                "field '{name}' has conflicting defaults across contracts"
                            ));
                        }
                    }
                }
            }
        }
        if let Some(conversation) = &self.conversation {
            for stage in conversation.stages.iter().chain(
                conversation
                    .overlays
                    .iter()
                    .flat_map(|overlay| overlay.stages.iter()),
            ) {
                if stage.frame.is_some() || !stage.resolve.is_empty() {
                    errors.push(format!("stage '{}' requires stage frame extraction or resolvers, which task skills do not yet support; use skill-level extract for owned speech fields", stage.id));
                }
                if stage.verbatim.is_some() || stage.timing.is_some() {
                    errors.push(format!("stage '{}' requires task-owned transcript or voice timing projection, which task skills do not yet support", stage.id));
                }
            }
            for policy in &conversation.policies {
                match policy {
                    crate::policy::Policy::Redact { .. } => {
                        errors.push("task skills do not yet provide redaction across input steering, approvals and results; redact policies are unsupported".into());
                    }
                    crate::policy::Policy::Commit { tool, .. } => {
                        if !self.tools.iter().any(|t| {
                            t.tool.name == *tool
                                && matches!(t.effect, TaskToolEffect::Commit { .. })
                        }) {
                            errors.push(format!(
                                "commit policy '{tool}' needs a commit effect declaration"
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        if !self.tests.is_empty() && self.flow.is_none() && self.conversation.is_none() {
            errors.push("skill tests require a governed flow or conversation; use task_scenarios for lifecycle checks".into());
        }
        let mut validation = self
            .definition()
            .validate_definition(http_support, &self.inputs.keys().cloned().collect());
        errors.extend(validation.errors);
        validation.valid = errors.is_empty();
        validation.errors = errors;
        if self.memory.is_some() {
            for name in super::MEMORY_TOOL_NAMES {
                if !validation.tools.iter().any(|tool| tool == name) {
                    validation.tools.push(name.into());
                }
            }
        }
        validation
    }

    /// Run the existing governed-workflow tests. Task approvals and lifecycle
    /// require session task scenarios, which exercise the task runtime.
    pub fn run_tests(&self) -> Vec<super::TestReport> {
        self.definition().run_tests()
    }

    /// Compile independent activation governance and invocation-local tool bindings.
    pub fn compile(&self, resources: &SpecResources) -> Result<CompiledSkill, String> {
        self.compile_mode(resources, false)
    }

    pub(super) fn compile_mode(
        &self,
        resources: &SpecResources,
        offline: bool,
    ) -> Result<CompiledSkill, String> {
        self.validate_with_http_support(!offline)
            .map_err(|e| e.join("; "))?;
        let definition = self.definition();
        let services = self.compile_services(resources, offline)?;
        let memory_enabled = self.memory.is_some();
        let flow_factory: Option<gemini_adk_rs::tasks::FlowFactory> =
            if let Some(conversation) = &self.conversation {
                let compiled = crate::conversation::Conversation::from_spec_stubbing_resolvers(
                    conversation.clone(),
                )
                .map_err(|e| e.to_string())?;
                Some(Arc::new(move || {
                    let stack = compiled.stack(Enforcement::Enforce);
                    Ok(if memory_enabled {
                        stack.with_ambient_tools(super::MEMORY_TOOL_NAMES.map(str::to_owned))
                    } else {
                        stack
                    })
                }))
            } else if let Some(flow) = &self.flow {
                let mut flow = flow.clone();
                if memory_enabled {
                    flow.ambient
                        .extend(super::MEMORY_TOOL_NAMES.map(str::to_owned));
                }
                let compiled = flow.compile().map_err(|e| e.to_string())?;
                Some(Arc::new(move || {
                    Ok(gemini_adk_rs::flow::FlowStack::new(
                        compiled.clone(),
                        Enforcement::Enforce,
                    ))
                }))
            } else {
                None
            };
        let mut initial_state = std::collections::HashMap::new();
        for (key, field) in &definition.state {
            if let Some(value) = &field.default {
                initial_state.insert(key.clone(), value.clone());
            }
        }
        let input_fields = self.inputs.clone();
        let output_fields = self.outputs.clone();
        let projected_fields = self.outputs.clone();
        let receipt_tools: BTreeMap<_, _> = self
            .tools
            .iter()
            .map(|declaration| (declaration.tool.name.clone(), declaration.tool.clone()))
            .collect();
        let mut skill = self.clone();
        let memory_tools = if self.memory.is_some() && !offline {
            resources
                .memory
                .as_ref()
                .ok_or("task memory requires SpecResources.memory")?
                .task_tools()
        } else {
            Vec::new()
        };
        if self.memory.is_some() {
            for name in super::MEMORY_TOOL_NAMES {
                let implementation = memory_tools.iter().find(|tool| tool.name() == name);
                if !offline && implementation.is_none() {
                    return Err(format!("task memory binding does not implement '{name}'"));
                }
                let effect = if name == "manage_memory" {
                    TaskToolEffect::Commit {
                        idempotency_argument: "request_id".into(),
                    }
                } else {
                    TaskToolEffect::Read
                };
                skill.tools.push(SkillToolSpec {
                    tool: ToolSpec {
                        name: name.into(),
                        description: implementation.map_or_else(
                            || format!("Controlled memory {name}"),
                            |tool| tool.description().to_owned(),
                        ),
                        parameters: implementation
                            .and_then(gemini_adk_rs::ToolFunction::parameters),
                        response: Some(json!({"fixture":true})),
                        set_state: BTreeMap::new(),
                        save_response_as: None,
                        http: None,
                        mcp: None,
                        background: false,
                        scheduling: None,
                    },
                    effect,
                });
            }
        }
        for declaration in &mut skill.tools {
            declaration.tool.parameters = task_parameters(declaration)?;
        }
        let tools = skill
            .tools
            .iter()
            .map(|t| TaskTool {
                name: t.tool.name.clone(),
                description: t.tool.description.clone(),
                parameters: t.tool.parameters.clone(),
                effect: t.effect.clone(),
            })
            .collect();
        let mut code: BTreeMap<String, Arc<dyn ToolFunction>> = if offline {
            BTreeMap::new()
        } else {
            self.tools
                .iter()
                .filter_map(|tool| {
                    resources
                        .tools
                        .get(&format!("{}/{}", self.name, tool.tool.name))
                        .or_else(|| resources.tools.get(&tool.tool.name))
                        .map(|implementation| (tool.tool.name.clone(), implementation.clone()))
                })
                .collect()
        };
        for tool in memory_tools {
            code.insert(tool.name().to_owned(), tool);
        }
        Ok(CompiledSkill {
            name: self.name.clone(),
            version: self.version.clone(),
            description: self.description.clone(),
            instruction: self.instruction.clone(),
            input_schema: field_schema(&self.inputs),
            output_schema: field_schema(&self.outputs),
            validate_input: Arc::new(move |value| validate_fields(&input_fields, value)),
            validate_output: Arc::new(move |value| validate_fields(&output_fields, value)),
            initial_state,
            flow_factory,
            services,
            tool_factory: Arc::new(move |state| skill.bind_tools(state, &code, offline)),
            tools,
            project_output: Some(Arc::new(move |state| {
                let value = Value::Object(
                    projected_fields
                        .keys()
                        .filter_map(|key| state.get_raw(key).map(|value| (key.clone(), value)))
                        .collect(),
                );
                validate_fields(&projected_fields, &value)?;
                Ok(value)
            })),
            receipt_applier: Some(Arc::new(move |name, result, state| {
                if memory_enabled && super::MEMORY_TOOL_NAMES.contains(&name) {
                    return Ok(());
                }
                let declaration = receipt_tools
                    .get(name)
                    .ok_or_else(|| format!("unknown receipt tool '{name}'"))?;
                apply_result_state(declaration, result, state)
            })),
        })
    }

    fn compile_services(
        &self,
        resources: &SpecResources,
        offline: bool,
    ) -> Result<Option<TaskServicesFactory>, String> {
        if self.extract.is_empty()
            && self.computed.is_empty()
            && self.watch.is_empty()
            && self.patterns.is_empty()
            && self.memory.is_none()
        {
            return Ok(None);
        }
        let llm = if self.extract.is_empty() {
            None
        } else if offline {
            // Controlled replay substitutes provider outputs after normal due selection.
            // This empty script makes any accidental provider call fail.
            Some(Arc::new(gemini_adk_rs::llm::MockLlm::script([]))
                as Arc<dyn gemini_adk_rs::llm::BaseLlm>)
        } else {
            Some(
                resources
                    .extraction_llm
                    .clone()
                    .ok_or("task extraction requires SpecResources.extraction_llm")?,
            )
        };
        let memory = self
            .memory
            .as_ref()
            .map(|memory| {
                let binding = resources
                    .memory
                    .as_ref()
                    .ok_or("task memory requires SpecResources.memory")?;
                let provider = binding.task_memory(memory)?;
                let fields = self.definition().state;
                Ok::<_, String>(Arc::new(ValidatedTaskMemory {
                    provider,
                    slots: memory.slots.clone(),
                    fields,
                })
                    as Arc<dyn gemini_adk_rs::tasks::TaskMemoryService>)
            })
            .transpose()?;
        let skill = self.clone();
        let factory: TaskServicesFactory = Arc::new(move || {
            let mut services = TaskServices {
                memory: memory.clone(),
                ..Default::default()
            };
            if let Some(llm) = &llm {
                services.extractors = skill
                    .extract
                    .iter()
                    .map(|e| {
                        Arc::new(super::compile_extractor(e, llm.clone()))
                            as Arc<dyn gemini_adk_rs::live::extractor::TurnExtractor>
                    })
                    .collect();
            }
            for spec in &skill.computed {
                let expression = spec.from.clone();
                services
                    .computed
                    .register(gemini_adk_rs::live::computed::ComputedVar {
                        key: spec.key.clone(),
                        dependencies: expression.keys_read().into_iter().collect(),
                        compute: Arc::new(move |state| expression.eval(state)),
                    })
                    .map_err(|error| error.to_string())?;
            }
            services.watchers = skill
                .watch
                .iter()
                .map(|watch| TaskWatcher {
                    key: watch.key.clone(),
                    predicate: match &watch.condition {
                        super::WatchCondition::Changed => {
                            gemini_adk_rs::live::watcher::WatchPredicate::Changed
                        }
                        super::WatchCondition::ChangedTo(v) => {
                            gemini_adk_rs::live::watcher::WatchPredicate::ChangedTo(v.clone())
                        }
                        super::WatchCondition::CrossedAbove(v) => {
                            gemini_adk_rs::live::watcher::WatchPredicate::CrossedAbove(*v)
                        }
                        super::WatchCondition::CrossedBelow(v) => {
                            gemini_adk_rs::live::watcher::WatchPredicate::CrossedBelow(*v)
                        }
                        super::WatchCondition::BecameTrue => {
                            gemini_adk_rs::live::watcher::WatchPredicate::BecameTrue
                        }
                        super::WatchCondition::BecameFalse => {
                            gemini_adk_rs::live::watcher::WatchPredicate::BecameFalse
                        }
                    },
                    effects: task_effects(&watch.set, &watch.effects),
                })
                .collect();
            for pattern in &skill.patterns {
                let guard = pattern.when.clone();
                let condition = Arc::new(move |state: &State| guard.eval_state(state));
                let detector: Box<dyn gemini_adk_rs::live::temporal::PatternDetector> =
                    match (pattern.sustained_secs, pattern.turns) {
                        (Some(seconds), None) => {
                            Box::new(gemini_adk_rs::live::temporal::SustainedDetector::new(
                                condition,
                                std::time::Duration::from_secs(seconds),
                            ))
                        }
                        (None, Some(turns)) => Box::new(
                            gemini_adk_rs::live::temporal::TurnCountDetector::new(condition, turns),
                        ),
                        _ => {
                            return Err(format!(
                                "pattern '{}' requires exactly one duration or turn count",
                                pattern.name
                            ));
                        }
                    };
                services.patterns.push(TaskPattern {
                    name: pattern.name.clone(),
                    detector,
                    cooldown: None,
                    effects: task_effects(&BTreeMap::new(), &pattern.effects),
                });
            }
            Ok(services)
        });
        // Validate the fresh registries now; errors must not wait until Start.
        factory()?;
        Ok(Some(factory))
    }

    fn bind_tools(
        &self,
        state: State,
        code: &BTreeMap<String, Arc<dyn ToolFunction>>,
        offline: bool,
    ) -> Result<ToolDispatcher, String> {
        let mut dispatcher = ToolDispatcher::new();
        for declaration in &self.tools {
            let tool = declaration.tool.clone();
            let implementation = code.get(&tool.name).cloned();
            let server = if offline {
                None
            } else {
                tool.mcp.as_ref().map(|params| {
                    Arc::new(gemini_adk_rs::tools::mcp::McpSessionManager::new(
                        crate::live::connect::parse_mcp_params(params),
                    ))
                })
            };
            let snapshot = state.clone();
            dispatcher.register(ContextTool::new(
                tool.name.clone(),
                tool.description.clone(),
                tool.parameters.clone(),
                move |args, mut ctx: ToolContext| {
                    let tool = tool.clone();
                    let implementation = implementation.clone();
                    let server = server.clone();
                    let snapshot = snapshot.clone();
                    async move {
                        ctx.state = snapshot.clone();
                        let result = if let Some(implementation) = implementation {
                            implementation.call_with_context(args, ctx).await?
                        } else if let Some(server) = server {
                            super::mcp_value(
                                server
                                    .call_tool(&tool.name, args)
                                    .await
                                    .map_err(|e| gemini_adk_rs::ToolError::Other(e.to_string()))?,
                            )
                        } else if let Some(binding) = tool.http.as_ref().filter(|_| !offline) {
                            super::execute_http(binding, &args, &snapshot)
                                .await
                                .map_err(gemini_adk_rs::ToolError::Other)?
                        } else {
                            tool.response.clone().unwrap_or_else(|| json!({"ok":true}))
                        };
                        apply_result_state(&tool, &result, &snapshot)
                            .map_err(gemini_adk_rs::ToolError::Other)?;
                        Ok(result)
                    }
                },
            ));
        }
        if let Some(conversation) = &self.conversation {
            let mut dispatcher = Some(dispatcher);
            crate::live::connect::apply_policies(&conversation.policies, &state, &mut dispatcher)
                .map_err(|e| e.to_string())?;
            Ok(dispatcher.expect("policy application retains dispatcher"))
        } else {
            Ok(dispatcher)
        }
    }

    pub(super) fn sandboxed(&self, allow: &super::BindingAllowlist) -> (Self, Vec<String>) {
        let (definition, notes) = self.definition().sandboxed(allow);
        let mut skill = self.clone();
        for (target, source) in skill.tools.iter_mut().zip(definition.tools) {
            target.tool = source;
        }
        (
            skill,
            notes
                .into_iter()
                .map(|note| format!("skill '{}': {note}", self.name))
                .collect(),
        )
    }
}

/// Apply only the declared state effects of an accepted success. Trusted
/// reconciliation uses this same adapter without executing the external tool.
fn apply_result_state(tool: &ToolSpec, result: &Value, state: &State) -> Result<(), String> {
    for (key, value) in &tool.set_state {
        state.set(key, value.clone()).map_err(|e| e.to_string())?;
    }
    if let Some(key) = &tool.save_response_as {
        state.set(key, result.clone()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(super) fn task_parameters(declaration: &SkillToolSpec) -> Result<Option<Value>, String> {
    let TaskToolEffect::Commit {
        idempotency_argument,
    } = &declaration.effect
    else {
        return Ok(declaration.tool.parameters.clone());
    };
    let invalid = || {
        format!(
            "commit tool '{}' needs object parameters with string idempotency field '{idempotency_argument}'",
            declaration.tool.name
        )
    };
    let mut parameters = declaration
        .tool
        .parameters
        .clone()
        .unwrap_or_else(|| json!({}));
    let object = parameters.as_object_mut().ok_or_else(invalid)?;
    if object.get("type").is_some_and(|kind| kind != "object") {
        return Err(invalid());
    }
    object.insert("type".into(), json!("object"));
    let properties = object
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(invalid)?;
    let field = properties
        .entry(idempotency_argument.clone())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(invalid)?;
    if field.get("type").is_some_and(|kind| kind != "string") {
        return Err(invalid());
    }
    field.insert("type".into(), json!("string"));
    field
        .entry("description")
        .or_insert_with(|| json!("Nonempty external idempotency key for this exact operation."));
    let required = object
        .entry("required")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(invalid)?;
    if !required.iter().all(Value::is_string) {
        return Err(invalid());
    }
    if !required.iter().any(|key| key == idempotency_argument) {
        required.push(json!(idempotency_argument));
    }
    Ok(Some(parameters))
}

fn field_schema(fields: &BTreeMap<String, StateFieldSpec>) -> Value {
    let properties: serde_json::Map<String, Value> = fields
        .iter()
        .map(|(name, field)| {
            let mut schema = json!({"description": field.description});
            if let Some(kind) = field.kind {
                schema["type"] = json!(match kind {
                    StateType::Boolean => "boolean",
                    StateType::Number => "number",
                    StateType::String => "string",
                    StateType::Object => "object",
                    StateType::Array => "array",
                });
            }
            if let Some(default) = &field.default {
                schema["default"] = default.clone();
            }
            (name.clone(), schema)
        })
        .collect();
    json!({"type":"object", "properties":properties, "required":fields.iter().filter(|(_, f)| f.default.is_none()).map(|(k, _)| k).collect::<Vec<_>>(), "additionalProperties":false})
}

fn validate_fields(fields: &BTreeMap<String, StateFieldSpec>, value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("contract expects an object")?;
    for key in object.keys() {
        if !fields.contains_key(key) {
            return Err(format!("unknown contract field '{key}'"));
        }
    }
    for (key, field) in fields {
        match object.get(key) {
            Some(value) if field.kind.is_some_and(|kind| !kind.matches(value)) => {
                return Err(format!("field '{key}' has the wrong type"));
            }
            None if field.default.is_none() => {
                return Err(format!("required field '{key}' is missing"));
            }
            _ => {}
        }
    }
    Ok(())
}

fn task_effects(sets: &BTreeMap<String, Value>, effects: &[super::EffectSpec]) -> TaskEffects {
    let sets = sets.clone();
    let effects = effects.to_vec();
    Arc::new(move |state| {
        let mut out: Vec<_> = sets
            .iter()
            .map(|(key, value)| TaskEffect::Set {
                key: key.clone(),
                value: value.clone(),
            })
            .collect();
        for effect in &effects {
            match effect {
                super::EffectSpec::Set(values) => {
                    out.extend(values.iter().map(|(key, value)| TaskEffect::Set {
                        key: key.clone(),
                        value: value.clone(),
                    }));
                }
                super::EffectSpec::Context(text) => out.push(TaskEffect::Context(text.clone())),
                super::EffectSpec::Prompt(text) => out.push(TaskEffect::Prompt(text.clone())),
                super::EffectSpec::Remember(template) => out.push(TaskEffect::Remember(
                    super::interpolate(template, &Value::Null, state),
                )),
            }
        }
        out
    })
}

struct ValidatedTaskMemory {
    provider: Arc<dyn gemini_adk_rs::tasks::TaskMemoryService>,
    slots: Vec<super::MemorySlotSpec>,
    fields: BTreeMap<String, StateFieldSpec>,
}
impl ValidatedTaskMemory {
    fn validate_projection(&self, value: Value) -> Result<Value, String> {
        let object = value
            .as_object()
            .ok_or("task memory projection must be an object")?;
        for (key, value) in object {
            if !self.slots.iter().any(|slot| slot.to == *key) {
                return Err(format!("task memory projected undeclared slot '{key}'"));
            }
            if !value.is_null()
                && self
                    .fields
                    .get(key)
                    .and_then(|field| field.kind)
                    .is_some_and(|kind| !kind.matches(value))
            {
                return Err(format!("task memory slot '{key}' has the wrong type"));
            }
        }
        Ok(value)
    }
}
#[async_trait::async_trait]
impl gemini_adk_rs::tasks::TaskMemoryService for ValidatedTaskMemory {
    async fn project(&self) -> Result<Value, String> {
        self.validate_projection(self.provider.project().await?)
    }
    async fn ingest(&self, id: String, turn: u64, user: String) -> Result<Value, String> {
        self.validate_projection(self.provider.ingest(id, turn, user).await?)
    }
    async fn remember(&self, id: String, turn: u64, note: String) -> Result<Value, String> {
        self.validate_projection(self.provider.remember(id, turn, note).await?)
    }
}

/// A deterministic journey through task commands and controlled tool effects.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskScenario {
    /// Display name used in CI and Studio.
    pub name: String,
    /// Events in order; task and operation IDs match the runtime snapshots.
    pub steps: Vec<TaskStep>,
}

/// One event applied by both scripted task tests and Studio Preview.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TaskStep {
    /// Apply a trusted command. Tools run with declared mock implementations.
    Command {
        /// The same command accepted by the live runtime.
        command: gemini_adk_rs::tasks::TaskCommand,
        /// Hold an admitted invocation until an explicit finish event.
        #[serde(default)]
        defer: bool,
        /// An anticipated admission error; no error means the step fails.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_error: Option<String>,
        /// Controlled results for due after-tool extractors.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extraction: BTreeMap<String, Value>,
        /// Controlled extraction provider failures.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extraction_errors: BTreeMap<String, String>,
    },
    /// Advance the same task-local governance as a live turn boundary.
    Turn,
    /// Execute and finish one previously deferred controlled invocation.
    Finish {
        /// Invocation identity from the runtime's operation snapshot.
        operation: gemini_adk_rs::tasks::OperationId,
        /// Controlled results for due after-tool extractors.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extraction: BTreeMap<String, Value>,
        /// Controlled extraction provider failures.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extraction_errors: BTreeMap<String, String>,
    },
    /// Observe owned speech through the same trigger/window/promotion pipeline.
    Observe {
        /// Activation receiving the observation.
        task: gemini_adk_rs::tasks::TaskId,
        /// Captured revision; omitted means the current revision at this step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<u64>,
        /// Observation kind, defaulting to finalized turn.
        #[serde(default)]
        trigger: TaskObservationKind,
        /// Finalized user text.
        #[serde(default)]
        user: String,
        /// Heard or generated model text, depending on trigger.
        #[serde(default)]
        model: String,
        /// Explicit results from the controlled extractor provider.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extraction: BTreeMap<String, Value>,
        /// Explicit provider failures, keyed by extractor name.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        extraction_errors: BTreeMap<String, String>,
        /// Hold prepared work until finish_service, allowing stale-result tests.
        #[serde(default)]
        defer: bool,
    },
    /// Complete a previously deferred service ticket.
    FinishService {
        /// Service operation identity from its owned ticket.
        operation: gemini_adk_rs::tasks::OperationId,
    },
    /// Advance the shared manual clock and observe a foreground timer tick.
    AdvanceTime {
        /// Elapsed milliseconds.
        millis: u64,
    },
    /// Seed the offline memory adapter, never authoritative task state.
    Memory {
        /// Skill whose controlled memory backend receives these fixtures.
        skill: String,
        /// Projection values keyed by declared slot target.
        values: BTreeMap<String, Value>,
    },
    /// Assert observable task state without exporting private task state.
    Expect {
        /// Foreground task ID, when it must be set to this value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        foreground: Option<gemini_adk_rs::tasks::TaskId>,
        /// Task observations that must hold.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tasks: Vec<TaskExpectation>,
        /// Operation outcomes that must hold.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        operations: BTreeMap<String, gemini_adk_rs::tasks::OperationStatus>,
    },
}

/// Serializable owned-observation vocabulary for task replay.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskObservationKind {
    /// Finalized user/heard-model turn.
    #[default]
    Turn,
    /// Generated model text before interruption truncation.
    GenerationComplete,
    /// Owned interruption.
    Interrupted,
    /// Owned timer tick.
    Timer,
}

/// An assertion about one task activation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TaskExpectation {
    /// Task ID to inspect.
    pub task: String,
    /// Expected activation lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<gemini_adk_rs::tasks::TaskStatus>,
    /// Expected pinned skill version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Expected input revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// Expected validated export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Active steps in the task's current governance layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active: Vec<String>,
    /// Completed steps in the task's current governance layer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done: Vec<String>,
}

/// One shared runtime snapshot after an offline task event.
#[derive(Debug, Clone, Serialize)]
pub struct TaskTraceSnapshot {
    /// Zero is the initial catalog before any command.
    pub index: usize,
    /// Human-readable event label.
    pub event: String,
    /// Failed assertions or unexpected command errors.
    pub failures: Vec<String>,
    /// Owner-labelled effects accepted by task services during this event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_effects: Vec<gemini_adk_rs::tasks::AcceptedTaskEffect>,
    /// Deferred service identities, allowing an explicit finish_service event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_services: Vec<gemini_adk_rs::tasks::TaskOwner>,
    /// The exact task observation forwarded by live sessions.
    #[serde(flatten)]
    pub status: gemini_adk_rs::tasks::TaskSessionSnapshot,
}

impl SessionSpec {
    /// Compile local skill definitions into the runtime used by Live.
    pub fn compile_tasks(
        &self,
        resources: &SpecResources,
    ) -> Result<gemini_adk_rs::tasks::TaskRuntime, String> {
        self.compile_tasks_mode(resources, false)
    }

    fn compile_tasks_mode(
        &self,
        resources: &SpecResources,
        offline: bool,
    ) -> Result<gemini_adk_rs::tasks::TaskRuntime, String> {
        let validation = if offline {
            self.validate_for_replay()
        } else {
            self.validate()
        };
        if !validation.valid {
            return Err(validation.errors.join("; "));
        }
        if self.skills.is_empty() {
            return Err("the session has no skills".into());
        }
        let skills = self
            .skills
            .iter()
            .map(|skill| skill.compile_mode(resources, offline))
            .collect::<Result<Vec<_>, _>>()?;
        gemini_adk_rs::tasks::TaskRuntime::new(skills).map_err(|e| e.to_string())
    }

    /// Replay commands with declared mock tools only. Attached implementations,
    /// HTTP bindings and MCP servers never execute in this path.
    pub async fn trace_tasks(
        &self,
        steps: &[TaskStep],
    ) -> Result<
        (
            Vec<TaskTraceSnapshot>,
            gemini_adk_rs::tasks::TaskSessionSnapshot,
        ),
        Vec<String>,
    > {
        self.trace_tasks_with_resources(steps, &SpecResources::default())
            .await
    }

    /// Replay with explicitly supplied controlled memory services. Tool implementations
    /// and extraction providers remain disabled; a supplied memory binding executes
    /// its project/ingest/remember methods and must be an operator-controlled fixture.
    pub async fn trace_tasks_with_resources(
        &self,
        steps: &[TaskStep],
        resources: &SpecResources,
    ) -> Result<
        (
            Vec<TaskTraceSnapshot>,
            gemini_adk_rs::tasks::TaskSessionSnapshot,
        ),
        Vec<String>,
    > {
        let validation = self.validate_for_replay();
        if !validation.valid {
            return Err(validation.errors);
        }
        if self.skills.is_empty() {
            return Err(vec!["the session has no skills".into()]);
        }
        let mut memory_fixtures = BTreeMap::new();
        let mut compiled = Vec::new();
        for skill in &self.skills {
            let mut bindings = resources.clone();
            if skill.memory.is_some() && bindings.memory.is_none() {
                let fixture = Arc::new(FixtureMemory::default());
                bindings.memory = Some(Arc::new(FixtureMemoryBinding(fixture.clone())));
                memory_fixtures.insert(skill.name.clone(), fixture);
            }
            compiled.push(
                skill
                    .compile_mode(&bindings, true)
                    .map_err(|error| vec![error])?,
            );
        }
        let clock = Arc::new(gemini_adk_rs::clock::ManualClock::new());
        let mut runtime = gemini_adk_rs::tasks::TaskRuntime::new(compiled)
            .map_err(|error| vec![error.to_string()])?
            .with_clock(clock.clone());
        let mut pending = BTreeMap::new();
        let mut pending_services: DeferredServices = BTreeMap::new();
        let mut snapshots = vec![TaskTraceSnapshot {
            index: 0,
            event: "initial".into(),
            failures: Vec::new(),
            service_effects: Vec::new(),
            deferred_services: Vec::new(),
            status: runtime.snapshot(),
        }];
        for (index, step) in steps.iter().enumerate() {
            let mut failures = Vec::new();
            let mut controlled = BTreeMap::new();
            let mut defer_services = false;
            let event = match step {
                TaskStep::Command {
                    command,
                    defer,
                    expected_error,
                    extraction,
                    extraction_errors,
                } => {
                    controlled = controlled_results(extraction, extraction_errors, &mut failures);
                    match runtime.command(command.clone()) {
                        Ok(invocation) => {
                            if let Some(expected) = expected_error {
                                failures.push(format!(
                                    "expected command error '{expected}', command succeeded"
                                ));
                            }
                            if let Some(invocation) = invocation {
                                if *defer {
                                    pending.insert(invocation.operation_id().clone(), invocation);
                                } else {
                                    runtime.complete(invocation.execute().await);
                                }
                            }
                        }
                        Err(error) => {
                            let message = error.to_string();
                            if !expected_error
                                .as_ref()
                                .is_some_and(|expected| message.contains(expected))
                            {
                                failures.push(message);
                            }
                        }
                    }
                    serde_json::to_value(command)
                        .ok()
                        .and_then(|v| v["action"].as_str().map(str::to_owned))
                        .unwrap_or_else(|| "command".into())
                }
                TaskStep::Turn => {
                    runtime.on_turn();
                    "turn".into()
                }
                TaskStep::Finish {
                    operation,
                    extraction,
                    extraction_errors,
                } => {
                    controlled = controlled_results(extraction, extraction_errors, &mut failures);
                    match pending.remove(operation) {
                        Some(invocation) => {
                            runtime.complete(invocation.execute().await);
                        }
                        None => failures.push(format!("no deferred invocation '{}'", operation.0)),
                    }
                    format!("finish: {}", operation.0)
                }
                TaskStep::Observe {
                    task,
                    revision,
                    trigger,
                    user,
                    model,
                    extraction,
                    extraction_errors,
                    defer,
                } => {
                    controlled = controlled_results(extraction, extraction_errors, &mut failures);
                    defer_services = *defer;
                    let revision = revision.or_else(|| {
                        runtime
                            .snapshot()
                            .tasks
                            .iter()
                            .find(|activation| activation.id == *task)
                            .map(|activation| activation.revision)
                    });
                    let event = match trigger {
                        TaskObservationKind::Turn => gemini_adk_rs::tasks::TaskObservation::Turn {
                            user: user.clone(),
                            model: model.clone(),
                        },
                        TaskObservationKind::GenerationComplete => {
                            gemini_adk_rs::tasks::TaskObservation::GenerationComplete {
                                user: user.clone(),
                                model: model.clone(),
                            }
                        }
                        TaskObservationKind::Interrupted => {
                            gemini_adk_rs::tasks::TaskObservation::Interrupted
                        }
                        TaskObservationKind::Timer => gemini_adk_rs::tasks::TaskObservation::Timer,
                    };
                    if let Some(revision) = revision {
                        if let Err(error) = runtime.observe(task, revision, event) {
                            failures.push(error.to_string());
                        }
                    } else {
                        failures.push(format!("task '{}' does not exist", task.0));
                    }
                    format!("observe: {}", task.0)
                }
                TaskStep::FinishService { operation } => {
                    match pending_services.remove(operation) {
                        Some((work, fixtures)) => {
                            runtime.complete_services(work.execute_controlled(fixtures).await);
                        }
                        None => failures.push(format!("no deferred service '{}'", operation.0)),
                    }
                    format!("finish_service: {}", operation.0)
                }
                TaskStep::AdvanceTime { millis } => {
                    clock.advance(std::time::Duration::from_millis(*millis));
                    let snapshot = runtime.snapshot();
                    if let Some(task) = snapshot
                        .tasks
                        .iter()
                        .find(|task| snapshot.foreground.as_ref() == Some(&task.id))
                        && let Err(error) = runtime.observe(
                            &task.id,
                            task.revision,
                            gemini_adk_rs::tasks::TaskObservation::Timer,
                        )
                    {
                        failures.push(error.to_string());
                    }
                    format!("advance_time: {millis}")
                }
                TaskStep::Memory { skill, values } => {
                    if let Some(fixture) = memory_fixtures.get(skill) {
                        let declaration = self
                            .skills
                            .iter()
                            .find(|candidate| candidate.name == *skill)
                            .and_then(|skill| skill.memory.as_ref());
                        if values.keys().any(|key| {
                            !declaration.is_some_and(|memory| {
                                memory.slots.iter().any(|slot| slot.to == *key)
                            })
                        }) {
                            failures.push(format!(
                                "memory fixture for '{skill}' contains an undeclared slot"
                            ));
                        } else {
                            *fixture.values.write().expect("fixture memory lock") = json!(values);
                        }
                    } else {
                        failures.push(format!("'{skill}' has no controlled replay memory adapter"));
                    }
                    format!("memory fixture: {skill}")
                }
                TaskStep::Expect {
                    foreground,
                    tasks,
                    operations,
                } => {
                    let snapshot = runtime.snapshot();
                    if let Some(expected) = foreground
                        && snapshot.foreground.as_ref() != Some(expected)
                    {
                        failures.push(format!(
                            "expected foreground '{}'; got {:?}",
                            expected.0, snapshot.foreground
                        ));
                    }
                    for expected in tasks {
                        let Some(task) = snapshot
                            .tasks
                            .iter()
                            .find(|task| task.id.0 == expected.task)
                        else {
                            failures.push(format!("task '{}' does not exist", expected.task));
                            continue;
                        };
                        if expected.status.is_some_and(|status| status != task.status) {
                            failures.push(format!(
                                "task '{}' status was {:?}",
                                expected.task, task.status
                            ));
                        }
                        if expected
                            .version
                            .as_ref()
                            .is_some_and(|version| version != &task.skill.version)
                        {
                            failures.push(format!(
                                "task '{}' version was '{}'",
                                expected.task, task.skill.version
                            ));
                        }
                        if expected
                            .revision
                            .is_some_and(|revision| revision != task.revision)
                        {
                            failures.push(format!(
                                "task '{}' revision was {}",
                                expected.task, task.revision
                            ));
                        }
                        if expected
                            .output
                            .as_ref()
                            .is_some_and(|output| Some(output) != task.output.as_ref())
                        {
                            failures.push(format!(
                                "task '{}' output was {:?}",
                                expected.task, task.output
                            ));
                        }
                        for step in &expected.active {
                            if !task
                                .flow
                                .as_ref()
                                .is_some_and(|flow| flow.explanation.active.contains(step))
                            {
                                failures.push(format!(
                                    "task '{}' expected active step '{step}'",
                                    expected.task
                                ));
                            }
                        }
                        for step in &expected.done {
                            if !task
                                .flow
                                .as_ref()
                                .is_some_and(|flow| flow.done.contains(step))
                            {
                                failures.push(format!(
                                    "task '{}' expected done step '{step}'",
                                    expected.task
                                ));
                            }
                        }
                    }
                    for (id, expected) in operations {
                        if !snapshot
                            .operations
                            .iter()
                            .any(|operation| operation.id.0 == *id && operation.status == *expected)
                        {
                            failures.push(format!("operation '{id}' expected {expected:?}"));
                        }
                    }
                    "expect".into()
                }
            };
            drain_services(
                &mut runtime,
                &mut controlled,
                defer_services,
                &mut pending_services,
            )
            .await;
            for name in controlled.keys() {
                failures.push(format!("unused controlled extraction '{name}'"));
            }
            let service_effects = runtime.take_service_effects();
            for effect in &service_effects {
                if let TaskEffect::Error(error) = &effect.effect {
                    failures.push(error.clone());
                }
            }
            snapshots.push(TaskTraceSnapshot {
                index: index + 1,
                event,
                failures,
                service_effects,
                deferred_services: pending_services
                    .values()
                    .map(|(work, _)| work.owner().clone())
                    .collect(),
                status: runtime.snapshot(),
            });
        }
        Ok((snapshots, runtime.snapshot()))
    }

    /// Run every declared task journey through the shared offline runtime.
    pub async fn run_task_scenarios(&self) -> Vec<super::ScenarioReport> {
        let mut reports = Vec::new();
        for scenario in &self.task_scenarios {
            let error = match self.trace_tasks(&scenario.steps).await {
                Ok((snapshots, _)) => snapshots
                    .into_iter()
                    .find(|snapshot| !snapshot.failures.is_empty())
                    .map(|snapshot| {
                        format!("event {}: {}", snapshot.index, snapshot.failures.join("; "))
                    }),
                Err(errors) => Some(errors.join("; ")),
            };
            reports.push(super::ScenarioReport {
                name: scenario.name.clone(),
                passed: error.is_none(),
                error,
            });
        }
        reports
    }
}

type ControlledExtraction = BTreeMap<String, Result<Value, String>>;
type DeferredServices = BTreeMap<
    gemini_adk_rs::tasks::OperationId,
    (
        gemini_adk_rs::tasks::OwnedTaskServiceWork,
        ControlledExtraction,
    ),
>;
fn controlled_results(
    values: &BTreeMap<String, Value>,
    errors: &BTreeMap<String, String>,
    failures: &mut Vec<String>,
) -> ControlledExtraction {
    let mut results: ControlledExtraction = values
        .iter()
        .map(|(name, value)| (name.clone(), Ok(value.clone())))
        .collect();
    for (name, error) in errors {
        if results.insert(name.clone(), Err(error.clone())).is_some() {
            failures.push(format!(
                "extractor '{name}' has both a result and an error fixture"
            ));
        }
    }
    results
}
async fn drain_services(
    runtime: &mut gemini_adk_rs::tasks::TaskRuntime,
    fixtures: &mut ControlledExtraction,
    defer: bool,
    pending: &mut DeferredServices,
) {
    loop {
        let jobs = runtime.take_ready_services();
        if jobs.is_empty() {
            break;
        }
        for work in jobs {
            let selected = work
                .extractor_names()
                .into_iter()
                .filter_map(|name| fixtures.remove_entry(name))
                .collect();
            if defer {
                pending.insert(work.owner().operation.clone(), (work, selected));
            } else {
                runtime.complete_services(work.execute_controlled(selected).await);
            }
        }
    }
}
#[derive(Default)]
struct FixtureMemory {
    values: std::sync::RwLock<Value>,
}
impl FixtureMemory {
    fn projection(&self) -> Value {
        let value = self.values.read().expect("fixture memory lock").clone();
        if value.is_object() { value } else { json!({}) }
    }
}
#[async_trait::async_trait]
impl gemini_adk_rs::tasks::TaskMemoryService for FixtureMemory {
    async fn project(&self) -> Result<Value, String> {
        Ok(self.projection())
    }
    async fn ingest(&self, _id: String, _turn: u64, _user: String) -> Result<Value, String> {
        Ok(self.projection())
    }
    async fn remember(&self, _id: String, _turn: u64, _note: String) -> Result<Value, String> {
        Ok(self.projection())
    }
}
struct FixtureMemoryBinding(Arc<FixtureMemory>);
impl super::MemoryBinding for FixtureMemoryBinding {
    fn install(&self, live: crate::live::Live, _memory: &super::MemorySpec) -> crate::live::Live {
        live
    }
    fn remember(&self, _note: String) {}
    fn task_memory(
        &self,
        _memory: &super::MemorySpec,
    ) -> Result<Arc<dyn gemini_adk_rs::tasks::TaskMemoryService>, String> {
        Ok(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_adk_rs::tasks::{TaskCommand, TaskId};

    fn simple() -> SessionSpec {
        SessionSpec::from_value(json!({
            "name":"support",
            "skills":[{"name":"faq","version":"1","instruction":"Answer.",
                "inputs":{"question":{"type":"string"}},
                "outputs":{"result":{"type":"object"}},
                "tools":[{"name":"lookup","effect":{"kind":"read"},
                    "response":{"answer":"help"},"save_response_as":"result"}]}]
        }))
        .expect("parses")
    }

    #[test]
    fn effect_policies_and_skill_versions_are_explicit() {
        let mut spec = simple();
        spec.skills[0].version.clear();
        assert!(!spec.validate().valid);
        let mut value = serde_json::to_value(simple()).expect("serializes");
        value["skills"][0]["tools"][0]
            .as_object_mut()
            .expect("tool")
            .remove("effect");
        assert!(SessionSpec::from_value(value).is_err());
    }

    #[test]
    fn authoring_and_runtime_share_provider_name_validation() {
        let mut spec = simple();
        spec.skills[0].name = "faq__nested".into();
        assert!(!spec.validate().valid);
        let mut spec = simple();
        spec.skills[0].tools[0].tool.name = "x".repeat(62);
        assert!(
            !spec.validate().valid,
            "qualified names are at most 64 bytes"
        );
        let mut spec = simple();
        spec.skills[0].tools[0].tool.background = true;
        assert!(
            spec.validate()
                .errors
                .iter()
                .any(|error| error.contains("delivery"))
        );
    }

    #[test]
    fn required_activation_inputs_are_writers_but_unproduced_state_still_warns() {
        let mut spec = simple();
        spec.skills[0].state.insert(
            "missing".into(),
            serde_json::from_value(json!({"type":"string"})).expect("declared field"),
        );
        spec.skills[0].flow = Some(
            serde_json::from_value(json!({
                "steps":[
                    {"id":"collect","allow":["lookup"],"done":{"captured":["question","missing"]}},
                    {"id":"done","after":["collect"],"terminal":true}
                ],
                "constraints":[{"require":["done"]}]
            }))
            .expect("flow"),
        );
        let validation = spec.validate();
        assert!(validation.valid, "{:?}", validation.errors);
        assert!(
            !validation
                .warnings
                .iter()
                .any(|warning| warning.contains("guard reads state key 'question'")),
            "a required activation input is populated by the runtime"
        );
        assert!(
            validation
                .warnings
                .iter()
                .any(|warning| warning.contains("guard reads state key 'missing'")),
            "declaring a non-input state field does not create a producer"
        );
        assert!(
            spec.skills[0]
                .definition()
                .validate()
                .warnings
                .iter()
                .any(|warning| warning.contains("guard reads state key 'question'")),
            "ordinary session state retains the missing-producer warning"
        );
    }

    #[test]
    fn commit_declarations_advertise_the_required_idempotency_argument() {
        let mut spec = simple();
        spec.skills[0].tools[0].effect = TaskToolEffect::Commit {
            idempotency_argument: "request_id".into(),
        };
        let compiled = spec.skills[0]
            .compile(&SpecResources::default())
            .expect("compiles");
        let parameters = compiled.tools[0]
            .parameters
            .as_ref()
            .expect("commit schema");
        assert_eq!(parameters["type"], "object");
        assert_eq!(parameters["properties"]["request_id"]["type"], "string");
        assert_eq!(parameters["required"], json!(["request_id"]));
        let dispatcher = (compiled.tool_factory)(State::new()).expect("binds");
        let gemini_adk_rs::tool::ToolKind::Function(tool) =
            dispatcher.get_tool("lookup").expect("tool")
        else {
            panic!("declared task tools are function tools");
        };
        assert_eq!(tool.parameters(), Some(parameters.clone()));
        spec.skills[0].tools[0].tool.parameters = Some(json!({
            "type":"object","properties":{"request_id":{"type":"number"}}
        }));
        assert!(!spec.validate().valid, "incompatible authored field fails");
    }

    #[test]
    fn contracts_and_ambiguous_global_pipelines_fail_at_load() {
        let mut spec = simple();
        spec.skills[0]
            .inputs
            .get_mut("question")
            .expect("field")
            .default = Some(json!(5));
        assert!(!spec.validate().valid);
        let mut spec = simple();
        spec.flow = Some(Flow::new().step("global").terminal().build().expect("flow"));
        assert!(
            spec.validate()
                .errors
                .iter()
                .any(|e| e.contains("session-level pipeline"))
        );
        let mut spec = simple();
        spec.state.insert(
            "question".into(),
            serde_json::from_value(json!({"type":"string","default":"root context"}))
                .expect("field"),
        );
        assert!(
            spec.validate()
                .errors
                .iter()
                .any(|error| error.contains("session-level pipeline"))
        );
    }

    #[test]
    fn overlapping_contracts_accept_matching_types_and_reject_conflicting_defaults() {
        let mut spec = simple();
        let field: StateFieldSpec =
            serde_json::from_value(json!({"type":"string","default":"same"})).expect("field");
        spec.skills[0]
            .inputs
            .insert("question".into(), field.clone());
        spec.skills[0]
            .outputs
            .insert("question".into(), field.clone());
        spec.skills[0].state.insert("question".into(), field);
        assert!(spec.validate().valid, "matching overlap is supported");
        spec.skills[0]
            .outputs
            .get_mut("question")
            .expect("field")
            .default = Some(json!("different"));
        assert!(
            spec.validate()
                .errors
                .iter()
                .any(|error| error.contains("conflicting defaults"))
        );
    }

    #[tokio::test]
    async fn replay_checks_inputs_and_declared_output_without_external_effects() {
        let spec = simple();
        let steps: Vec<TaskStep> = serde_json::from_value(json!([
            {"event":"command","command":{"action":"start","skill":"faq","input":{"question":5}},"expected_error":"wrong type"},
            {"event":"command","command":{"action":"start","skill":"faq","input":{"question":"Help"}}},
            {"event":"command","command":{"action":"complete","task":"task-1","output":{}},"expected_error":"required field"},
            {"event":"command","command":{"action":"invoke","task":"task-1","tool":"lookup","args":{}}},
            {"event":"command","command":{"action":"complete","task":"task-1","output":{"should_not_export":true}}},
            {"event":"expect","tasks":[{"task":"task-1","status":"completed","output":{"result":{"answer":"help"}}}]}
        ])).expect("steps");
        let (snapshots, status) = spec.trace_tasks(&steps).await.expect("traces");
        assert!(
            snapshots.iter().all(|s| s.failures.is_empty()),
            "{:?}",
            snapshots.iter().map(|s| &s.failures).collect::<Vec<_>>()
        );
        assert_eq!(
            status.tasks.len(),
            1,
            "rejected activation allocates no task"
        );
        let (initial, status) = spec.trace_tasks(&[]).await.expect("initial");
        assert_eq!(initial.len(), 1);
        assert_eq!(status.skills[0].key.version, "1");
    }

    #[tokio::test]
    async fn custom_tool_context_is_owned_and_bound_to_private_invocation_state() {
        let resources = SpecResources::default().implement_skill(
            "faq",
            ContextTool::new("lookup", "lookup", None, |args, ctx| async move {
                let owner = ctx.task.as_ref().expect("task ownership forwarded");
                let question: String = ctx
                    .state
                    .try_get("question")
                    .map_err(|e| gemini_adk_rs::ToolError::Other(e.to_string()))?
                    .ok_or_else(|| gemini_adk_rs::ToolError::Other("missing question".into()))?;
                assert!(args.is_object());
                Ok(json!({"task":owner.task.0,"question":question}))
            }),
        );
        let mut runtime = simple().compile_tasks(&resources).expect("compiles");
        for index in 1..=2 {
            runtime
                .command(TaskCommand::Start {
                    skill: "faq".into(),
                    input: json!({"question":format!("question-{index}")}),
                    parent: None,
                })
                .expect("starts");
            let id = TaskId(format!("task-{index}"));
            let invocation = runtime
                .command(TaskCommand::Invoke {
                    task: id.clone(),
                    tool: "lookup".into(),
                    args: json!({}),
                    idempotency_key: None,
                })
                .expect("admits")
                .expect("invocation");
            let delivery = runtime.complete(invocation.execute().await);
            assert!(!delivery.stale);
            runtime
                .command(TaskCommand::Complete {
                    task: id,
                    output: json!({}),
                })
                .expect("completes");
        }
        let snapshot = runtime.snapshot();
        assert_eq!(
            snapshot.tasks[0].output,
            Some(json!({"result":{"task":"task-1","question":"question-1"}}))
        );
        assert_eq!(
            snapshot.tasks[1].output,
            Some(json!({"result":{"task":"task-2","question":"question-2"}}))
        );
    }

    #[tokio::test]
    async fn reconciled_commit_applies_declared_state_without_reexecuting_tool() {
        use gemini_adk_rs::tasks::{OperationStatus, ReconciledOutcome};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mut spec = simple();
        spec.skills[0].tools[0].effect = TaskToolEffect::Commit {
            idempotency_argument: "request_id".into(),
        };
        spec.skills[0].tools[0]
            .tool
            .set_state
            .insert("accepted".into(), json!(true));
        spec.skills[0].outputs.insert(
            "accepted".into(),
            serde_json::from_value(json!({"type":"boolean"})).expect("field"),
        );
        spec.skills[0].outputs.insert(
            "unverified".into(),
            serde_json::from_value(json!({"type":"boolean","default":false})).expect("field"),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let resources = SpecResources::default().implement_skill(
            "faq",
            ContextTool::new("lookup", "lookup", None, move |_args, ctx| {
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    ctx.state
                        .set("unverified", true)
                        .map_err(|error| gemini_adk_rs::ToolError::Other(error.to_string()))?;
                    Err(gemini_adk_rs::ToolError::Other("connection lost".into()))
                }
            }),
        );
        let mut runtime = spec.compile_tasks(&resources).expect("compiles");
        runtime
            .command(TaskCommand::Start {
                skill: "faq".into(),
                input: json!({"question":"Help"}),
                parent: None,
            })
            .expect("starts");
        runtime
            .command(TaskCommand::Invoke {
                task: TaskId("task-1".into()),
                tool: "lookup".into(),
                args: json!({"request_id":"receipt-1"}),
                idempotency_key: None,
            })
            .expect("awaits approval");
        let operation = runtime.snapshot().operations[0].id.clone();
        let invocation = runtime
            .command(TaskCommand::Decide {
                operation: operation.clone(),
                approve: true,
            })
            .expect("approves")
            .expect("invocation");
        runtime.complete(invocation.execute().await);
        assert_eq!(
            runtime.snapshot().operations[0].status,
            OperationStatus::Unknown
        );
        let verified = json!({"receipt":"externally-verified"});
        runtime
            .command(TaskCommand::Reconcile {
                operation,
                outcome: ReconciledOutcome::Succeeded {
                    result: verified.clone(),
                },
            })
            .expect("reconstructs accepted state");
        runtime
            .command(TaskCommand::Complete {
                task: TaskId("task-1".into()),
                output: json!({}),
            })
            .expect("exports reconstructed response");
        assert_eq!(
            runtime.snapshot().tasks[0].output,
            Some(json!({"result":verified,"accepted":true,"unverified":false}))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "reconciliation is pure");
    }

    #[tokio::test]
    async fn support_demo_runs_shared_task_journeys() {
        let spec: SessionSpec = serde_json::from_str(include_str!(
            "../../../../apps/gemini-adk-web-rs/static/examples/flows/support-agent.json"
        ))
        .expect("demo parses");
        let validation = spec.validate();
        assert!(validation.valid, "{:?}", validation.errors);
        assert_eq!(validation.steps, 5, "billing and diagnostics governance");
        assert_eq!(
            validation.tools,
            [
                "billing__verify_account",
                "billing__submit_adjustment",
                "faq__search_faq",
                "diagnostics__run_diagnostics",
                "handoff__transfer_case",
            ]
        );
        for report in spec.run_task_scenarios().await {
            assert!(report.passed, "{}: {:?}", report.name, report.error);
        }
    }

    #[test]
    fn session_workflow_tests_include_qualified_skill_passes_and_failures() {
        let mut spec = simple();
        spec.skills[0].flow = Some(
            serde_json::from_value(json!({
                "steps":[
                    {"id":"retrieve","allow":["lookup"],"done":{"called_ok":"lookup"}},
                    {"id":"done","after":["retrieve"],"terminal":true}
                ],
                "constraints":[{"require":["done"]}]
            }))
            .expect("governed flow"),
        );
        spec.skills[0].tests = serde_json::from_value(json!([
            {"name":"retrieval","script":[{"tool":"lookup"},{"expect":{"complete":true}}]},
            {"name":"unfilled","script":[{"expect":{"complete":true}}]}
        ]))
        .expect("workflow tests");
        assert!(spec.validate().valid);
        let reports = spec.run_tests();
        assert_eq!(reports.len(), 2, "each child test runs once");
        assert_eq!(reports[0].name, "faq/retrieval");
        assert!(reports[0].passed, "{:?}", reports[0].failures);
        assert_eq!(reports[1].name, "faq/unfilled");
        assert!(!reports[1].passed);
        assert!(!reports[1].failures.is_empty());
        assert_eq!(spec.skills[0].run_tests()[0].name, "retrieval");
    }

    #[test]
    fn recursive_sandbox_preserves_effect_policy_and_mock_state_writes() {
        let mut spec = simple();
        spec.skills[0].tools[0].tool.http = Some(
            serde_json::from_value(json!({"url":"http://127.0.0.1:1/blocked"})).expect("binding"),
        );
        let (safe, notes) = spec.sandboxed(&super::super::BindingAllowlist::default());
        assert!(safe.skills[0].tools[0].tool.http.is_none());
        assert!(matches!(
            safe.skills[0].tools[0].effect,
            TaskToolEffect::Read
        ));
        assert_eq!(
            safe.skills[0].tools[0].tool.save_response_as.as_deref(),
            Some("result")
        );
        assert!(notes[0].contains("skill 'faq'"));
    }

    #[test]
    fn unsupported_task_owned_extraction_and_redaction_fail_explicitly() {
        let mut spec = simple();
        spec.skills[0].conversation = Some(
            serde_json::from_value(json!({
                "name":"unsupported",
                "stages":[{"id":"resolve","resolve":[{"slot":"answer","resolver":"lookup"}]}],
                "policies":[{"kind":"redact","keys":["question"]}]
            }))
            .expect("conversation"),
        );
        let errors = spec.skills[0]
            .validate()
            .expect_err("unsupported behavior must fail");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("stage frame extraction"))
        );
        assert!(errors.iter().any(|error| error.contains("redaction")));
    }

    #[tokio::test]
    async fn offline_validation_keeps_conflicting_bindings_visible() {
        let mut spec = simple();
        spec.skills[0].tools[0].tool.http = Some(
            serde_json::from_value(json!({"url":"http://127.0.0.1:1/no-network"}))
                .expect("binding"),
        );
        spec.skills[0].tools[0].tool.mcp = Some("would-run-an-external-command".into());
        let errors = spec
            .trace_tasks(&[])
            .await
            .expect_err("conflicting bindings fail before mock replay");
        assert!(errors[0].contains("both an http and an mcp binding"));
    }
    fn service_spec() -> SessionSpec {
        SessionSpec::from_value(json!({
            "name":"owned-services", "skills":[{
                "name":"intake","version":"1",
                "outputs":{"name":{"type":"string"},"urgent":{"type":"boolean"},
                    "ready":{"type":"boolean"},"noticed":{"type":"boolean"},"stuck":{"type":"boolean"}},
                "state":{"urgent":{"type":"boolean","default":false},
                    "noticed":{"type":"boolean","default":false},"stuck":{"type":"boolean","default":false}},
                "extract":[{"name":"details","instruction":"Extract the name and urgency.","window":2,
                    "schema":{"type":"object","properties":{"person":{"type":"string"},"urgent":{"type":"boolean"}}},
                    "promote":[{"field":"person","to":"name"},{"field":"urgent","policy":"true_only"}]}],
                "computed":[{"key":"ready","from":{"key":"urgent"}}],
                "watch":[{"key":"ready","condition":"became_true","set":{"noticed":true},
                    "effects":[{"context":"Urgency has been captured."},{"prompt":"Confirm the callback."}]}],
                "patterns":[{"name":"two-urgent-turns","when":{"is_true":"urgent"},"turns":2,
                    "effects":[{"set":{"stuck":true}}]}]
            }]
        })).expect("service spec")
    }

    async fn replay_service_steps(spec: &SessionSpec, steps: Value) -> Vec<TaskTraceSnapshot> {
        let steps: Vec<TaskStep> = serde_json::from_value(steps).expect("service steps");
        let (trace, _) = spec.trace_tasks(&steps).await.expect("service replay");
        assert!(
            trace.iter().all(|snapshot| snapshot.failures.is_empty()),
            "{trace:#?}"
        );
        trace
    }

    #[tokio::test]
    async fn task_speech_uses_promotions_computed_watchers_and_turn_patterns() {
        let spec = service_spec();
        assert!(spec.requires_extraction());
        assert!(!spec.requires_memory());
        assert!(spec.validate().valid, "{:?}", spec.validate().errors);
        assert!(
            spec.compile_tasks(&SpecResources::default())
                .err()
                .expect("missing resources")
                .contains("extraction_llm")
        );
        let trace = replay_service_steps(&spec,json!([
            {"event":"command","command":{"action":"start","skill":"intake"}},
            {"event":"observe","task":"task-1","user":"My name is Ada and this is urgent",
                "extraction":{"details":{"person":"Ada","urgent":true}}},
            {"event":"observe","task":"task-1","user":"I actually meant another name but urgency remains",
                "extraction":{"details":{"person":"Grace","urgent":false}}},
            {"event":"command","command":{"action":"complete","task":"task-1","output":{}}},
            {"event":"expect","tasks":[{"task":"task-1","status":"completed",
                "output":{"name":"Ada","urgent":true,"ready":true,"noticed":true,"stuck":true}}]}
        ])).await;
        let context_count = trace
            .iter()
            .flat_map(|snapshot| &snapshot.service_effects)
            .filter(|effect| matches!(effect.effect, TaskEffect::Context(_)))
            .count();
        assert_eq!(
            context_count, 1,
            "computed watcher fires once for the accepted change"
        );
    }

    #[tokio::test]
    async fn task_service_replay_keeps_minimum_words_and_reports_unused_fixtures() {
        let spec = service_spec();
        let steps: Vec<TaskStep> = serde_json::from_value(json!([
            {"event":"command","command":{"action":"start","skill":"intake"}},
            {"event":"observe","task":"task-1","user":"Hi","extraction":{"details":{"person":"Ada"}}}
        ])).expect("steps");
        let (trace, _) = spec.trace_tasks(&steps).await.expect("trace");
        assert!(
            trace[2]
                .failures
                .iter()
                .any(|failure| failure.contains("unused controlled extraction"))
        );
        assert!(
            trace[2].service_effects.is_empty(),
            "skipped extraction never promotes fixtures"
        );
    }

    #[tokio::test]
    async fn task_service_replay_rejects_late_revision_results_and_isolates_activations() {
        let mut spec = service_spec();
        spec.skills[0].outputs.clear();
        let trace = replay_service_steps(&spec,json!([
            {"event":"command","command":{"action":"start","skill":"intake"}},
            {"event":"observe","task":"task-1","user":"This is urgent and my name is Ada",
                "extraction":{"details":{"person":"Ada","urgent":true}},"defer":true},
            {"event":"command","command":{"action":"revise","task":"task-1","expected_revision":1,"input":{}}},
            {"event":"finish_service","operation":"service-2"},
            {"event":"command","command":{"action":"start","skill":"intake","parent":"task-1"}},
            {"event":"observe","task":"task-2","user":"My name is Grace and it is not urgent",
                "extraction":{"details":{"person":"Grace","urgent":false}}},
            {"event":"command","command":{"action":"complete","task":"task-2","output":{}}},
            {"event":"expect","foreground":"task-1","tasks":[{"task":"task-1","revision":2,"status":"running"}]}
        ])).await;
        assert!(
            !trace
                .iter()
                .flat_map(|snapshot| &snapshot.service_effects)
                .any(|effect| matches!(effect.effect, TaskEffect::Prompt(_))),
            "discarded worker does not fire watchers"
        );
    }

    #[tokio::test]
    async fn task_generation_and_after_tool_extraction_use_their_declared_triggers() {
        let mut value = serde_json::to_value(service_spec()).expect("serialize");
        let skill = &mut value["skills"][0];
        skill["computed"] = json!([]);
        skill["watch"] = json!([]);
        skill["patterns"] = json!([]);
        skill["outputs"] = json!({"name":{"type":"string"},"urgent":{"type":"boolean"}});
        skill["tools"] =
            json!([{ "name":"lookup","effect":{"kind":"read"},"response":{"ok":true}}]);
        skill["extract"][0]["trigger"] = json!("on_generation_complete");
        let mut after = skill["extract"][0].clone();
        after["name"] = json!("after_lookup");
        after["trigger"] = json!("after_tool_call");
        skill["extract"]
            .as_array_mut()
            .expect("extract")
            .push(after);
        let spec = SessionSpec::from_value(value).expect("spec");
        replay_service_steps(&spec,json!([
            {"event":"command","command":{"action":"start","skill":"intake"}},
            {"event":"observe","task":"task-1","user":"My name is Ada and not urgent"},
            {"event":"observe","task":"task-1","trigger":"generation_complete","user":"My name is Ada and not urgent",
                "model":"I can help you","extraction":{"details":{"person":"Ada","urgent":false}}},
            {"event":"command","command":{"action":"invoke","task":"task-1","tool":"lookup","args":{}},
                "extraction":{"after_lookup":{"person":"Grace","urgent":true}}},
            {"event":"command","command":{"action":"complete","task":"task-1","output":{}}},
            {"event":"expect","tasks":[{"task":"task-1","output":{"name":"Ada","urgent":true}}]}
        ])).await;
    }

    #[tokio::test]
    async fn task_memory_fixtures_project_only_declared_typed_slots() {
        let spec = SessionSpec::from_value(json!({"name":"memory","skills":[{
            "name":"profile","version":"1","memory":{"slots":[{"predicate":"preferred_name","to":"name"}]},
            "outputs":{"name":{"type":"string"}}
        }]})).expect("spec");
        assert!(spec.requires_memory());
        assert!(
            spec.compile_tasks(&SpecResources::default())
                .err()
                .expect("missing resources")
                .contains("SpecResources.memory")
        );
        let trace = replay_service_steps(
            &spec,
            json!([
                {"event":"memory","skill":"profile","values":{"name":"Ada"}},
                {"event":"command","command":{"action":"start","skill":"profile"}},
                {"event":"command","command":{"action":"complete","task":"task-1","output":{}}},
                {"event":"expect","tasks":[{"task":"task-1","output":{"name":"Ada"}}]}
            ]),
        )
        .await;
        let catalog = &trace[0].status.skills[0];
        assert_eq!(catalog.tools.len(), 2);
        assert!(matches!(
            catalog
                .tools
                .iter()
                .find(|tool| tool.name == "manage_memory")
                .expect("memory tool")
                .effect,
            TaskToolEffect::Commit { .. }
        ));
        let wrong: Vec<TaskStep> = serde_json::from_value(json!([
            {"event":"memory","skill":"profile","values":{"name":5}},
            {"event":"command","command":{"action":"start","skill":"profile"}}
        ]))
        .expect("steps");
        let (trace, _) = spec.trace_tasks(&wrong).await.expect("trace");
        assert!(
            trace[2]
                .failures
                .iter()
                .any(|failure| failure.contains("wrong type"))
        );
    }

    #[tokio::test]
    async fn task_temporal_service_uses_manual_time_and_fresh_activation_detectors() {
        let spec = SessionSpec::from_value(json!({"name":"temporal","skills":[{
            "name":"wait","version":"1","inputs":{"waiting":{"type":"boolean"}},
            "outputs":{"nudged":{"type":"boolean"}},"state":{"nudged":{"type":"boolean","default":false}},
            "patterns":[{"name":"waiting-five-seconds","when":{"is_true":"waiting"},"sustained_secs":5,
                "effects":[{"set":{"nudged":true}}]}]
        }]})).expect("spec");
        replay_service_steps(&spec,json!([
            {"event":"command","command":{"action":"start","skill":"wait","input":{"waiting":true}}},
            {"event":"observe","task":"task-1","trigger":"timer"},
            {"event":"advance_time","millis":5000},
            {"event":"command","command":{"action":"complete","task":"task-1","output":{}}},
            {"event":"expect","tasks":[{"task":"task-1","output":{"nudged":true}}]},
            {"event":"command","command":{"action":"start","skill":"wait","input":{"waiting":true}}},
            {"event":"observe","task":"task-2","trigger":"timer"},
            {"event":"command","command":{"action":"complete","task":"task-2","output":{}}},
            {"event":"expect","tasks":[{"task":"task-2","output":{"nudged":false}}]}
        ])).await;
    }

    #[test]
    fn task_services_roundtrip_and_reject_phase_triggers_before_activation() {
        let spec = service_spec();
        let encoded = serde_json::to_value(&spec).expect("serialize");
        let roundtrip = SessionSpec::from_value(encoded.clone()).expect("parse");
        assert_eq!(
            serde_json::to_value(&roundtrip).expect("serialize"),
            encoded
        );
        let mut spec = roundtrip;
        spec.skills[0].extract[0].trigger = super::super::TriggerSpec::OnPhaseChange;
        assert!(
            spec.validate()
                .errors
                .iter()
                .any(|error| error.contains("on_phase_change"))
        );
    }
    #[test]
    fn task_memory_installs_shared_lifecycle_once_for_multiple_skills() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Binding {
            installs: Arc<AtomicUsize>,
        }
        impl super::super::MemoryBinding for Binding {
            fn install(
                &self,
                _live: crate::live::Live,
                _memory: &super::super::MemorySpec,
            ) -> crate::live::Live {
                panic!("task mode must not install global memory services")
            }
            fn remember(&self, _note: String) {
                panic!("task remember uses owned work")
            }
            fn task_memory(
                &self,
                _memory: &super::super::MemorySpec,
            ) -> Result<Arc<dyn gemini_adk_rs::tasks::TaskMemoryService>, String> {
                Ok(Arc::new(FixtureMemory::default()))
            }
            fn task_tools(&self) -> Vec<Arc<dyn ToolFunction>> {
                super::super::MEMORY_TOOL_NAMES
                    .into_iter()
                    .map(|name| {
                        Arc::new(gemini_adk_rs::tool::SimpleTool::new(
                            name,
                            "Controlled memory",
                            None,
                            |_| async { Ok(json!({})) },
                        )) as Arc<dyn ToolFunction>
                    })
                    .collect()
            }
            fn install_task_lifecycle(&self, live: crate::live::Live) -> crate::live::Live {
                self.installs.fetch_add(1, Ordering::SeqCst);
                live
            }
        }
        let spec = SessionSpec::from_value(json!({"name":"memory","skills":[
            {"name":"profile","version":"1","memory":{}},
            {"name":"preferences","version":"1","memory":{}}
        ]}))
        .expect("spec");
        let installs = Arc::new(AtomicUsize::new(0));
        let resources = SpecResources {
            memory: Some(Arc::new(Binding {
                installs: installs.clone(),
            })),
            ..Default::default()
        };
        assert!(
            spec.apply(crate::live::Live::builder(), &State::new(), &resources)
                .is_ok()
        );
        assert_eq!(installs.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn task_services_cannot_author_writes_into_runtime_storage() {
        let mut spec = service_spec();
        spec.skills[0].extract[0].promote[0].to = Some("task:input".into());
        spec.skills[0].watch[0]
            .set
            .insert("flow:done:gate".into(), json!(true));
        spec.skills[0].patterns[0]
            .effects
            .push(super::super::EffectSpec::Set(BTreeMap::from([(
                "derived:ready".into(),
                json!(true),
            )])));
        let errors = spec.validate().errors;
        for key in ["task:input", "flow:done:gate", "derived:ready"] {
            assert!(errors.iter().any(|error| error.contains(key)), "{errors:?}");
        }
    }
}
