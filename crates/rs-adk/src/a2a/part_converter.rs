//! Bidirectional GenAI <-> A2A Part conversion.

use rs_genai::prelude::{Blob, CodeExecutionResult, ExecutableCode, FunctionCall, FunctionResponse, Part};
use super::types::{A2aFileContent, A2aPart};
use std::collections::HashMap;

/// Metadata key identifying the ADK type of a data part.
pub const ADK_TYPE_KEY: &str = "adk_type";
/// Metadata key flagging a function call as long-running.
pub const ADK_IS_LONG_RUNNING_KEY: &str = "adk_is_long_running";
/// Metadata key for thought/reasoning content.
pub const ADK_THOUGHT_KEY: &str = "adk_thought";

/// Type tag for function call data parts.
pub const DATA_TYPE_FUNCTION_CALL: &str = "function_call";
/// Type tag for function response data parts.
pub const DATA_TYPE_FUNCTION_RESPONSE: &str = "function_response";
/// Type tag for code execution result data parts.
pub const DATA_TYPE_CODE_EXEC_RESULT: &str = "code_execution_result";
/// Type tag for executable code data parts.
pub const DATA_TYPE_EXECUTABLE_CODE: &str = "executable_code";

/// Convert GenAI Parts to A2A Parts.
pub fn to_a2a_parts(parts: &[Part], long_running_tool_ids: &[String]) -> Vec<A2aPart> {
    parts
        .iter()
        .filter_map(|p| to_a2a_part(p, long_running_tool_ids))
        .collect()
}

/// Convert a single GenAI Part to an A2A Part.
pub fn to_a2a_part(part: &Part, long_running_tool_ids: &[String]) -> Option<A2aPart> {
    match part {
        Part::Text { text } => Some(A2aPart::Text {
            text: text.clone(),
            metadata: None,
        }),
        Part::InlineData { inline_data } => Some(A2aPart::File {
            file: A2aFileContent {
                name: None,
                mime_type: Some(inline_data.mime_type.clone()),
                bytes: Some(inline_data.data.clone()),
                uri: None,
            },
            metadata: None,
        }),
        Part::FunctionCall { function_call } => {
            let mut metadata = HashMap::new();
            metadata.insert(
                ADK_TYPE_KEY.to_string(),
                serde_json::json!(DATA_TYPE_FUNCTION_CALL),
            );
            if let Some(id) = &function_call.id {
                if long_running_tool_ids.contains(id) {
                    metadata.insert(
                        ADK_IS_LONG_RUNNING_KEY.to_string(),
                        serde_json::json!(true),
                    );
                }
            }
            Some(A2aPart::Data {
                data: serde_json::json!({
                    "name": function_call.name,
                    "args": function_call.args,
                    "id": function_call.id,
                }),
                metadata: Some(metadata),
            })
        }
        Part::FunctionResponse { function_response } => {
            let mut metadata = HashMap::new();
            metadata.insert(
                ADK_TYPE_KEY.to_string(),
                serde_json::json!(DATA_TYPE_FUNCTION_RESPONSE),
            );
            Some(A2aPart::Data {
                data: serde_json::json!({
                    "name": function_response.name,
                    "response": function_response.response,
                    "id": function_response.id,
                }),
                metadata: Some(metadata),
            })
        }
        Part::ExecutableCode { executable_code } => {
            let mut metadata = HashMap::new();
            metadata.insert(
                ADK_TYPE_KEY.to_string(),
                serde_json::json!(DATA_TYPE_EXECUTABLE_CODE),
            );
            Some(A2aPart::Data {
                data: serde_json::json!({
                    "language": executable_code.language,
                    "code": executable_code.code,
                }),
                metadata: Some(metadata),
            })
        }
        Part::CodeExecutionResult {
            code_execution_result,
        } => {
            let mut metadata = HashMap::new();
            metadata.insert(
                ADK_TYPE_KEY.to_string(),
                serde_json::json!(DATA_TYPE_CODE_EXEC_RESULT),
            );
            Some(A2aPart::Data {
                data: serde_json::json!({
                    "outcome": code_execution_result.outcome,
                    "output": code_execution_result.output,
                }),
                metadata: Some(metadata),
            })
        }
    }
}

/// Convert A2A Parts to GenAI Parts.
pub fn to_genai_parts(a2a_parts: &[A2aPart]) -> Vec<Part> {
    a2a_parts.iter().filter_map(to_genai_part).collect()
}

/// Convert a single A2A Part to a GenAI Part.
pub fn to_genai_part(a2a_part: &A2aPart) -> Option<Part> {
    match a2a_part {
        A2aPart::Text { text, .. } => Some(Part::Text { text: text.clone() }),
        A2aPart::File { file, .. } => {
            // Convert to InlineData if bytes present; URI-based files can't be represented
            file.bytes.as_ref().map(|bytes| Part::InlineData {
                inline_data: Blob {
                    mime_type: file.mime_type.clone().unwrap_or_default(),
                    data: bytes.clone(),
                },
            })
        }
        A2aPart::Data { data, metadata } => {
            let adk_type = metadata
                .as_ref()
                .and_then(|m| m.get(ADK_TYPE_KEY))
                .and_then(|v| v.as_str());

            match adk_type {
                Some(DATA_TYPE_FUNCTION_CALL) => Some(Part::FunctionCall {
                    function_call: FunctionCall {
                        name: data.get("name")?.as_str()?.to_string(),
                        args: data.get("args").cloned().unwrap_or(serde_json::json!({})),
                        id: data.get("id").and_then(|v| v.as_str()).map(String::from),
                    },
                }),
                Some(DATA_TYPE_FUNCTION_RESPONSE) => Some(Part::FunctionResponse {
                    function_response: FunctionResponse {
                        name: data.get("name")?.as_str()?.to_string(),
                        response: data
                            .get("response")
                            .cloned()
                            .unwrap_or(serde_json::json!({})),
                        id: data.get("id").and_then(|v| v.as_str()).map(String::from),
                        scheduling: None,
                    },
                }),
                Some(DATA_TYPE_EXECUTABLE_CODE) => Some(Part::ExecutableCode {
                    executable_code: ExecutableCode {
                        language: data.get("language")?.as_str()?.to_string(),
                        code: data.get("code")?.as_str()?.to_string(),
                    },
                }),
                Some(DATA_TYPE_CODE_EXEC_RESULT) => Some(Part::CodeExecutionResult {
                    code_execution_result: CodeExecutionResult {
                        outcome: data.get("outcome")?.as_str()?.to_string(),
                        output: data
                            .get("output")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    },
                }),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_part_to_a2a() {
        let part = Part::Text {
            text: "hello world".to_string(),
        };
        let a2a = to_a2a_part(&part, &[]).unwrap();
        match a2a {
            A2aPart::Text { text, metadata } => {
                assert_eq!(text, "hello world");
                assert!(metadata.is_none());
            }
            _ => panic!("Expected Text part"),
        }
    }

    #[test]
    fn inline_data_to_a2a_file() {
        let part = Part::InlineData {
            inline_data: Blob {
                mime_type: "image/png".to_string(),
                data: "base64data".to_string(),
            },
        };
        let a2a = to_a2a_part(&part, &[]).unwrap();
        match a2a {
            A2aPart::File { file, metadata } => {
                assert_eq!(file.mime_type.as_deref(), Some("image/png"));
                assert_eq!(file.bytes.as_deref(), Some("base64data"));
                assert!(file.uri.is_none());
                assert!(file.name.is_none());
                assert!(metadata.is_none());
            }
            _ => panic!("Expected File part"),
        }
    }

    #[test]
    fn function_call_to_a2a_data_with_metadata() {
        let part = Part::FunctionCall {
            function_call: FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "London"}),
                id: Some("call-1".to_string()),
            },
        };
        let a2a = to_a2a_part(&part, &[]).unwrap();
        match &a2a {
            A2aPart::Data { data, metadata } => {
                assert_eq!(data["name"], "get_weather");
                assert_eq!(data["args"]["city"], "London");
                assert_eq!(data["id"], "call-1");
                let meta = metadata.as_ref().unwrap();
                assert_eq!(meta[ADK_TYPE_KEY], DATA_TYPE_FUNCTION_CALL);
                assert!(!meta.contains_key(ADK_IS_LONG_RUNNING_KEY));
            }
            _ => panic!("Expected Data part"),
        }
    }

    #[test]
    fn function_call_long_running() {
        let part = Part::FunctionCall {
            function_call: FunctionCall {
                name: "slow_op".to_string(),
                args: serde_json::json!({}),
                id: Some("lr-1".to_string()),
            },
        };
        let long_running = vec!["lr-1".to_string()];
        let a2a = to_a2a_part(&part, &long_running).unwrap();
        match &a2a {
            A2aPart::Data { metadata, .. } => {
                let meta = metadata.as_ref().unwrap();
                assert_eq!(meta[ADK_IS_LONG_RUNNING_KEY], serde_json::json!(true));
            }
            _ => panic!("Expected Data part"),
        }
    }

    #[test]
    fn function_response_to_a2a_data() {
        let part = Part::FunctionResponse {
            function_response: FunctionResponse {
                name: "get_weather".to_string(),
                response: serde_json::json!({"temp": 20}),
                id: Some("call-1".to_string()),
                scheduling: None,
            },
        };
        let a2a = to_a2a_part(&part, &[]).unwrap();
        match &a2a {
            A2aPart::Data { data, metadata } => {
                assert_eq!(data["name"], "get_weather");
                assert_eq!(data["response"]["temp"], 20);
                assert_eq!(data["id"], "call-1");
                let meta = metadata.as_ref().unwrap();
                assert_eq!(meta[ADK_TYPE_KEY], DATA_TYPE_FUNCTION_RESPONSE);
            }
            _ => panic!("Expected Data part"),
        }
    }

    #[test]
    fn executable_code_to_a2a_data() {
        let part = Part::ExecutableCode {
            executable_code: ExecutableCode {
                language: "python".to_string(),
                code: "print('hi')".to_string(),
            },
        };
        let a2a = to_a2a_part(&part, &[]).unwrap();
        match &a2a {
            A2aPart::Data { data, metadata } => {
                assert_eq!(data["language"], "python");
                assert_eq!(data["code"], "print('hi')");
                let meta = metadata.as_ref().unwrap();
                assert_eq!(meta[ADK_TYPE_KEY], DATA_TYPE_EXECUTABLE_CODE);
            }
            _ => panic!("Expected Data part"),
        }
    }

    #[test]
    fn code_execution_result_to_a2a_data() {
        let part = Part::CodeExecutionResult {
            code_execution_result: CodeExecutionResult {
                outcome: "success".to_string(),
                output: Some("hi".to_string()),
            },
        };
        let a2a = to_a2a_part(&part, &[]).unwrap();
        match &a2a {
            A2aPart::Data { data, metadata } => {
                assert_eq!(data["outcome"], "success");
                assert_eq!(data["output"], "hi");
                let meta = metadata.as_ref().unwrap();
                assert_eq!(meta[ADK_TYPE_KEY], DATA_TYPE_CODE_EXEC_RESULT);
            }
            _ => panic!("Expected Data part"),
        }
    }

    #[test]
    fn a2a_text_to_genai_part() {
        let a2a = A2aPart::Text {
            text: "hello".to_string(),
            metadata: None,
        };
        let part = to_genai_part(&a2a).unwrap();
        match part {
            Part::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("Expected Text part"),
        }
    }

    #[test]
    fn a2a_file_to_genai_inline_data() {
        let a2a = A2aPart::File {
            file: A2aFileContent {
                name: None,
                mime_type: Some("audio/pcm".to_string()),
                bytes: Some("pcmdata".to_string()),
                uri: None,
            },
            metadata: None,
        };
        let part = to_genai_part(&a2a).unwrap();
        match part {
            Part::InlineData { inline_data } => {
                assert_eq!(inline_data.mime_type, "audio/pcm");
                assert_eq!(inline_data.data, "pcmdata");
            }
            _ => panic!("Expected InlineData part"),
        }
    }

    #[test]
    fn a2a_file_uri_only_returns_none() {
        let a2a = A2aPart::File {
            file: A2aFileContent {
                name: None,
                mime_type: Some("image/png".to_string()),
                bytes: None,
                uri: Some("gs://bucket/img.png".to_string()),
            },
            metadata: None,
        };
        assert!(to_genai_part(&a2a).is_none());
    }

    #[test]
    fn a2a_data_function_call_to_genai() {
        let mut metadata = HashMap::new();
        metadata.insert(
            ADK_TYPE_KEY.to_string(),
            serde_json::json!(DATA_TYPE_FUNCTION_CALL),
        );
        let a2a = A2aPart::Data {
            data: serde_json::json!({
                "name": "search",
                "args": {"query": "rust"},
                "id": "fc-1",
            }),
            metadata: Some(metadata),
        };
        let part = to_genai_part(&a2a).unwrap();
        match part {
            Part::FunctionCall { function_call } => {
                assert_eq!(function_call.name, "search");
                assert_eq!(function_call.args["query"], "rust");
                assert_eq!(function_call.id.as_deref(), Some("fc-1"));
            }
            _ => panic!("Expected FunctionCall part"),
        }
    }

    #[test]
    fn a2a_data_function_response_to_genai() {
        let mut metadata = HashMap::new();
        metadata.insert(
            ADK_TYPE_KEY.to_string(),
            serde_json::json!(DATA_TYPE_FUNCTION_RESPONSE),
        );
        let a2a = A2aPart::Data {
            data: serde_json::json!({
                "name": "search",
                "response": {"results": [1, 2, 3]},
                "id": "fc-1",
            }),
            metadata: Some(metadata),
        };
        let part = to_genai_part(&a2a).unwrap();
        match part {
            Part::FunctionResponse { function_response } => {
                assert_eq!(function_response.name, "search");
                assert_eq!(function_response.response["results"], serde_json::json!([1, 2, 3]));
                assert_eq!(function_response.id.as_deref(), Some("fc-1"));
            }
            _ => panic!("Expected FunctionResponse part"),
        }
    }

    #[test]
    fn a2a_data_executable_code_to_genai() {
        let mut metadata = HashMap::new();
        metadata.insert(
            ADK_TYPE_KEY.to_string(),
            serde_json::json!(DATA_TYPE_EXECUTABLE_CODE),
        );
        let a2a = A2aPart::Data {
            data: serde_json::json!({
                "language": "python",
                "code": "x = 1",
            }),
            metadata: Some(metadata),
        };
        let part = to_genai_part(&a2a).unwrap();
        match part {
            Part::ExecutableCode { executable_code } => {
                assert_eq!(executable_code.language, "python");
                assert_eq!(executable_code.code, "x = 1");
            }
            _ => panic!("Expected ExecutableCode part"),
        }
    }

    #[test]
    fn a2a_data_code_exec_result_to_genai() {
        let mut metadata = HashMap::new();
        metadata.insert(
            ADK_TYPE_KEY.to_string(),
            serde_json::json!(DATA_TYPE_CODE_EXEC_RESULT),
        );
        let a2a = A2aPart::Data {
            data: serde_json::json!({
                "outcome": "success",
                "output": "42",
            }),
            metadata: Some(metadata),
        };
        let part = to_genai_part(&a2a).unwrap();
        match part {
            Part::CodeExecutionResult {
                code_execution_result,
            } => {
                assert_eq!(code_execution_result.outcome, "success");
                assert_eq!(code_execution_result.output.as_deref(), Some("42"));
            }
            _ => panic!("Expected CodeExecutionResult part"),
        }
    }

    #[test]
    fn a2a_data_unknown_type_returns_none() {
        let a2a = A2aPart::Data {
            data: serde_json::json!({"foo": "bar"}),
            metadata: None,
        };
        assert!(to_genai_part(&a2a).is_none());
    }

    // Round-trip tests

    #[test]
    fn round_trip_text() {
        let original = Part::Text {
            text: "round trip".to_string(),
        };
        let a2a = to_a2a_part(&original, &[]).unwrap();
        let back = to_genai_part(&a2a).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn round_trip_function_call() {
        let original = Part::FunctionCall {
            function_call: FunctionCall {
                name: "my_tool".to_string(),
                args: serde_json::json!({"x": 10}),
                id: Some("id-1".to_string()),
            },
        };
        let a2a = to_a2a_part(&original, &[]).unwrap();
        let back = to_genai_part(&a2a).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn round_trip_function_response() {
        let original = Part::FunctionResponse {
            function_response: FunctionResponse {
                name: "my_tool".to_string(),
                response: serde_json::json!({"result": "ok"}),
                id: Some("id-1".to_string()),
                scheduling: None,
            },
        };
        let a2a = to_a2a_part(&original, &[]).unwrap();
        let back = to_genai_part(&a2a).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn to_a2a_parts_filters_and_collects() {
        let parts = vec![
            Part::Text {
                text: "a".to_string(),
            },
            Part::Text {
                text: "b".to_string(),
            },
        ];
        let a2a = to_a2a_parts(&parts, &[]);
        assert_eq!(a2a.len(), 2);
    }

    #[test]
    fn to_genai_parts_filters_and_collects() {
        let a2a_parts = vec![
            A2aPart::Text {
                text: "x".to_string(),
                metadata: None,
            },
            // This data part has no adk_type, so it will be filtered out
            A2aPart::Data {
                data: serde_json::json!({}),
                metadata: None,
            },
        ];
        let genai = to_genai_parts(&a2a_parts);
        assert_eq!(genai.len(), 1);
    }
}
