//! Data-driven flow applications — compatibility surface over
//! [`gemini_adk_fluent_rs::spec`].
//!
//! The spec vocabulary now lives in the SDK (`gemini-adk-fluent-rs::spec`)
//! as [`SessionSpec`]: the whole session — flow, tools (mock/HTTP/MCP),
//! extraction, phases, watchers, fragments, and embedded tests — as one
//! serializable JSON document. This module re-exports it under the original
//! `flow_app` names so existing server-side callers keep compiling, and old
//! `FlowAppSpec` JSON documents keep parsing (the spec is a strict superset).

pub use gemini_adk_fluent_rs::spec::{
    EffectSpec, ExtractSpec, HttpBinding, PhaseSpec, PromotePolicy, PromoteSpec, SessionSpec,
    SimEvent, SpecModality, SpecResources, SpecTest, SpecValidation, TestExpectation, TestReport,
    ToolSpec, TransitionSpec, TriggerSpec, UseFragment, WatchCondition, WatchSpec, run_tests,
};

/// The original name for the app document. Alias of [`SessionSpec`]; every
/// old `FlowAppSpec` JSON document parses unchanged.
pub type FlowAppSpec = SessionSpec;
/// The original name for a declared tool. Alias of [`ToolSpec`].
pub type MockToolSpec = ToolSpec;
/// The original name for the modality. Alias of [`SpecModality`].
pub type FlowModality = SpecModality;
/// The original name for the validation result. Alias of [`SpecValidation`].
pub type FlowValidation = SpecValidation;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The exact document shape shipped before the spec moved into the SDK
    /// must keep parsing and validating identically.
    #[test]
    fn legacy_flow_app_documents_still_parse() {
        let spec = FlowAppSpec::from_value(json!({
            "name": "collections",
            "instruction": "You collect payments.",
            "modality": "text",
            "tools": [
                {"name": "verify_identity", "set_state": {"identity_verified": true}},
                {"name": "charge_card", "response": {"charged": true}}
            ],
            "flow": {
                "steps": [
                    {"id": "verify", "posture": "Verify the caller.",
                     "allow": ["verify_identity"],
                     "done": {"is_true": "identity_verified"}},
                    {"id": "pay", "after": ["verify"], "posture": "Take payment.",
                     "allow": ["charge_card"],
                     "done": {"called_ok": "charge_card"}}
                ],
                "constraints": [
                    {"never_until": {"tool": "charge_card",
                                     "until": {"is_true": "identity_verified"}}}
                ]
            }
        }))
        .expect("legacy document parses");
        let v = spec.validate();
        assert!(v.valid, "errors: {:?}", v.errors);
        assert_eq!(v.steps, 2);
        assert!(v.mermaid.contains("verify --> pay"));
    }

    #[test]
    fn bare_flows_still_wrap() {
        let spec = FlowAppSpec::from_value(json!({
            "steps": [{"id": "only", "terminal": true}]
        }))
        .expect("bare flow parses");
        assert!(spec.validate().valid);
    }
}
