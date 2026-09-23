//! An application that depends on `gemini-adk` alone gets the whole golden
//! path — including `#[tool]`, whose expansion has to find the runtime
//! through this crate.

use gemini_adk::prelude::*;
use gemini_adk::testing::{LlmResponse, MockLlm};

/// Double a number.
///
/// # Arguments
///
/// * `n` - The number to double.
#[tool]
async fn double(n: i64) -> i64 {
    n * 2
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct Answer {
    value: i64,
}

#[tokio::test]
async fn the_golden_path_works_through_the_facade() {
    let llm = MockLlm::script([
        LlmResponse::tool_call("double", serde_json::json!({ "n": 21 })),
        LlmResponse::from_text(r#"{"value": 42}"#),
    ]);
    let agent = AgentBuilder::new("calc")
        .instruction("Use your tools.")
        .tool(double())
        .build(llm.clone())
        .unwrap();

    let answer: Answer = agent.ask_as("Double 21.").await.unwrap();
    assert_eq!(answer.value, 42);

    let declared = &llm.requests()[0].tools;
    assert!(
        declared.iter().any(|t| t
            .function_declarations
            .iter()
            .flatten()
            .any(|f| f.name == "double" && f.description == "Double a number.")),
        "{declared:?}"
    );
}
