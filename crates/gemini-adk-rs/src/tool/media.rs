//! Tool media returns — the ADK pattern where a function response carries
//! images or other media to the model alongside its JSON payload, so
//! vision tools (screenshots, chart renderers, document croppers) feed
//! the model something it can actually look at.
//!
//! The mechanism is a convention, so it works with every existing tool
//! shape unchanged: a tool embeds media under the reserved `"_media"` key
//! of its JSON result (via [`attach`]), and the text-agent loop lifts it
//! out of the function response and delivers it to the model as
//! `inline_data` parts in the same turn. Tools that never touch `_media`
//! behave exactly as before.
//!
//! ```ignore
//! T::simple("chart", "Render a chart", |args| async move {
//!     let png: Vec<u8> = render(&args)?;
//!     let mut result = json!({"rendered": true});
//!     media::attach(&mut result, "image/png", &png);
//!     Ok(result)
//! })
//! ```

use base64::Engine as _;

/// Reserved result key holding media attachments.
pub const MEDIA_KEY: &str = "_media";

/// One media attachment lifted from a tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAttachment {
    /// MIME type, e.g. `"image/png"`.
    pub mime_type: String,
    /// Base64-encoded payload, ready for an `inline_data` part.
    pub data_base64: String,
}

/// Attach raw media bytes to a tool result. Repeated calls accumulate.
pub fn attach(result: &mut serde_json::Value, mime_type: &str, data: &[u8]) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
    let entry = serde_json::json!({"mime_type": mime_type, "data": encoded});
    match result {
        serde_json::Value::Object(map) => {
            map.entry(MEDIA_KEY)
                .or_insert_with(|| serde_json::Value::Array(Vec::new()))
                .as_array_mut()
                .map(|list| list.push(entry));
        }
        other => {
            // Non-object results are wrapped so the attachment has a home.
            let wrapped = serde_json::json!({"result": other.take(), MEDIA_KEY: [entry]});
            *other = wrapped;
        }
    }
}

/// Remove and return any media attachments from a tool result — called by
/// the agent loop so the model receives media as parts, not as base64 JSON
/// noise inside the function response.
pub fn extract(result: &mut serde_json::Value) -> Vec<MediaAttachment> {
    let Some(map) = result.as_object_mut() else {
        return Vec::new();
    };
    let Some(raw) = map.remove(MEDIA_KEY) else {
        return Vec::new();
    };
    raw.as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some(MediaAttachment {
                mime_type: entry.get("mime_type")?.as_str()?.to_string(),
                data_base64: entry.get("data")?.as_str()?.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn attach_then_extract_round_trips() {
        let mut result = json!({"ok": true});
        attach(&mut result, "image/png", b"\x89PNG");
        attach(&mut result, "image/jpeg", b"\xff\xd8");
        let media = extract(&mut result);
        assert_eq!(media.len(), 2);
        assert_eq!(media[0].mime_type, "image/png");
        assert_eq!(
            media[0].data_base64,
            base64::engine::general_purpose::STANDARD.encode(b"\x89PNG")
        );
        // The reserved key is gone; the payload is untouched.
        assert_eq!(result, json!({"ok": true}));
    }

    #[test]
    fn non_object_results_are_wrapped() {
        let mut result = json!("plain text");
        attach(&mut result, "image/png", b"x");
        assert_eq!(result["result"], json!("plain text"));
        assert_eq!(extract(&mut result).len(), 1);
    }

    #[test]
    fn extract_without_media_is_a_no_op() {
        let mut result = json!({"ok": true});
        assert!(extract(&mut result).is_empty());
        assert_eq!(result, json!({"ok": true}));
    }
}
