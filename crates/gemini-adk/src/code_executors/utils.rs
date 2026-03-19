use gemini_live::prelude::{CodeExecutionResult as GenaiCodeExecResult, ExecutableCode, Part};

/// Extract the first code block from text content using the given delimiters.
///
/// Returns `Some((code, remaining_text))` where `code` is the extracted code
/// and `remaining_text` is the original text with the code block removed.
/// Returns `None` if no code block is found.
pub fn extract_code_from_text(
    text: &str,
    delimiters: &[(String, String)],
) -> Option<(String, String)> {
    for (start_delim, end_delim) in delimiters {
        if let Some(start_idx) = text.find(start_delim.as_str()) {
            let code_start = start_idx + start_delim.len();
            if let Some(end_idx) = text[code_start..].find(end_delim.as_str()) {
                let code = text[code_start..code_start + end_idx].to_string();
                let remaining = format!(
                    "{}{}",
                    &text[..start_idx],
                    &text[code_start + end_idx + end_delim.len()..]
                );
                return Some((code, remaining));
            }
        }
    }
    None
}

/// Build an `ExecutableCode` Part (language=PYTHON).
pub fn build_executable_code_part(code: &str) -> Part {
    Part::ExecutableCode {
        executable_code: ExecutableCode {
            language: "PYTHON".to_string(),
            code: code.to_string(),
        },
    }
}

/// Build a `CodeExecutionResult` Part with OK or FAILED outcome.
///
/// If `stderr` is empty the outcome is "OK" and the output is `stdout`.
/// Otherwise the outcome is "FAILED" and the output combines both streams.
pub fn build_code_execution_result_part(stdout: &str, stderr: &str) -> Part {
    let outcome = if stderr.is_empty() { "OK" } else { "FAILED" };
    let output = if stderr.is_empty() {
        stdout.to_string()
    } else {
        format!("{}\n{}", stdout, stderr)
    };
    Part::CodeExecutionResult {
        code_execution_result: GenaiCodeExecResult {
            outcome: outcome.to_string(),
            output: Some(output),
        },
    }
}
