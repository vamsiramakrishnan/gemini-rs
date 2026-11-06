//! Small text helpers shared across the workspace: code-fence stripping,
//! character-safe truncation, `{key}` templating and edit distance.

use serde_json::Value;

/// `text` without a surrounding Markdown code fence, for parsing the JSON a
/// model wrapped in one.
///
/// Handles `` ```json\n…\n``` ``, a bare `` ```\n…\n``` ``, a fence on one
/// line (`` ```{"a":1}``` ``), an unclosed fence, any language tag
/// (`` ```JSON ``), and surrounding whitespace. Text with no fence comes back
/// trimmed.
///
/// ```
/// use gemini_adk_rs::utils::text::strip_code_fence;
///
/// assert_eq!(strip_code_fence("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
/// assert_eq!(strip_code_fence("  {\"a\": 1}  "), "{\"a\": 1}");
/// ```
pub fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let body = match rest.split_once('\n') {
        // The opening line is only a language tag (or empty): drop it.
        Some((tag, body)) if is_fence_tag(tag) => body,
        // Content starts on the fence line: ```{"a":1}``` or ```json {"a":1}```.
        _ => strip_json_tag(rest),
    };
    let body = body.trim_end();
    body.strip_suffix("```").unwrap_or(body).trim()
}

/// Whether the text after an opening fence is just a language tag.
fn is_fence_tag(tag: &str) -> bool {
    tag.trim()
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '.' | '#'))
}

/// Drop a leading `json` tag when the fence and the content share a line.
fn strip_json_tag(rest: &str) -> &str {
    match rest.get(..4) {
        Some(tag)
            if tag.eq_ignore_ascii_case("json")
                && !rest[4..].starts_with(|c: char| c.is_ascii_alphanumeric()) =>
        {
            &rest[4..]
        }
        _ => rest,
    }
}

/// The first `max_chars` characters of `s`, cut on a character boundary.
///
/// Unlike `&s[..n]`, this cannot panic on a multi-byte character.
///
/// ```
/// use gemini_adk_rs::utils::text::truncate_chars;
///
/// assert_eq!(truncate_chars("héllo wörld", 5), "héllo");
/// assert_eq!(truncate_chars("short", 50), "short");
/// ```
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `s`, or its first `max_chars` characters followed by `marker` when it is
/// longer. The result is `max_chars` plus the marker at most.
///
/// ```
/// use gemini_adk_rs::utils::text::truncate_with_marker;
///
/// assert_eq!(truncate_with_marker("héllo wörld", 5, "…"), "héllo…");
/// assert_eq!(truncate_with_marker("héllo", 5, "…"), "héllo");
/// ```
pub fn truncate_with_marker(s: &str, max_chars: usize, marker: &str) -> String {
    let head = truncate_chars(s, max_chars);
    if head.len() == s.len() {
        s.to_string()
    } else {
        format!("{head}{marker}")
    }
}

/// Replace each `{expr}` in `template` with the value `lookup` gives for
/// `expr` (trimmed): a string verbatim, any other JSON value as its JSON
/// text. An unknown `expr` becomes the empty string; an unclosed `{` is kept
/// as written.
///
/// The scanner is shared; what `expr` means is the caller's. `Orchestration`
/// looks it up as a state key, the spec layer reads `args.` and `state.`
/// prefixes.
///
/// ```
/// use gemini_adk_rs::utils::text::interpolate_with;
/// use serde_json::json;
///
/// let out = interpolate_with("hi {name}, n={n}, {gone}!", |key| match key {
///     "name" => Some(json!("Ada")),
///     "n" => Some(json!(3)),
///     _ => None,
/// });
/// assert_eq!(out, "hi Ada, n=3, !");
/// ```
pub fn interpolate_with(template: &str, mut lookup: impl FnMut(&str) -> Option<Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        match lookup(after[..close].trim()) {
            Some(Value::String(s)) => out.push_str(&s),
            Some(v) => out.push_str(&v.to_string()),
            None => {}
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Edit distance between `a` and `b`, counted in characters (insert, delete
/// and substitute each cost 1).
///
/// ```
/// use gemini_adk_rs::utils::text::levenshtein;
///
/// assert_eq!(levenshtein("kitten", "sitting"), 3);
/// assert_eq!(levenshtein("héllo", "hello"), 1);
/// ```
pub fn levenshtein(a: &str, b: &str) -> usize {
    strsim::levenshtein(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fence_with_a_json_tag() {
        assert_eq!(strip_code_fence("```json\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_code_fence("```JSON\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(
            strip_code_fence("```json\r\n{\"a\": 1}\r\n```"),
            "{\"a\": 1}"
        );
    }

    #[test]
    fn bare_fence() {
        assert_eq!(strip_code_fence("```\n{\"a\": 1}\n```"), "{\"a\": 1}");
        assert_eq!(strip_code_fence("```\n[1, 2]\n```"), "[1, 2]");
    }

    #[test]
    fn no_fence() {
        assert_eq!(strip_code_fence("{\"a\": 1}"), "{\"a\": 1}");
        assert_eq!(strip_code_fence("plain words"), "plain words");
        assert_eq!(strip_code_fence(""), "");
    }

    #[test]
    fn surrounding_whitespace() {
        assert_eq!(strip_code_fence("  \n{\"a\": 1}\n  "), "{\"a\": 1}");
        assert_eq!(
            strip_code_fence("\n\n  ```json\n  {\"a\": 1}  \n```  \n\n"),
            "{\"a\": 1}"
        );
    }

    #[test]
    fn single_line_and_unclosed_fences() {
        assert_eq!(strip_code_fence("```{\"a\": 1}```"), "{\"a\": 1}");
        assert_eq!(strip_code_fence("```json{\"a\": 1}```"), "{\"a\": 1}");
        assert_eq!(strip_code_fence("```json {\"a\": 1}```"), "{\"a\": 1}");
        assert_eq!(strip_code_fence("```json\n{\"a\": 1}"), "{\"a\": 1}");
        // A value that merely starts with the letters of a tag is content.
        assert_eq!(strip_code_fence("```jsonish```"), "jsonish");
        // A fence whose first line is content, not a tag, keeps that line.
        assert_eq!(strip_code_fence("```{\n\"a\": 1\n}\n```"), "{\n\"a\": 1\n}");
    }

    #[test]
    fn truncate_chars_cuts_on_a_character_boundary() {
        // 'é' is two bytes and '日' three: a byte slice at 2 or 4 would panic.
        let s = "é日本語です";
        assert_eq!(truncate_chars(s, 2), "é日");
        assert_eq!(truncate_chars(s, 0), "");
        assert_eq!(truncate_chars(s, 6), s);
        assert_eq!(truncate_chars(s, 100), s);
        assert_eq!(truncate_chars("", 3), "");
    }

    #[test]
    fn truncate_with_marker_marks_only_a_cut() {
        let s = "日本語のテキスト";
        assert_eq!(truncate_with_marker(s, 3, "..."), "日本語...");
        assert_eq!(truncate_with_marker(s, 8, "..."), s);
        assert_eq!(truncate_with_marker("abc", 2, "…"), "ab…");
    }

    /// The CLI's old byte-slicing `truncate` (`&s[..max - 3]` with max 80)
    /// panicked here: byte 77 falls inside a three-byte character.
    #[test]
    fn a_multibyte_string_that_byte_slicing_would_have_panicked_on() {
        let s = "日".repeat(100); // 300 bytes
        assert!(!s.is_char_boundary(77));
        let cut = truncate_with_marker(&s, 77, "...");
        assert_eq!(cut.chars().count(), 80);
        assert!(cut.ends_with("日..."));
    }

    #[test]
    fn interpolate_with_substitutes_strings_and_json() {
        let out = interpolate_with("a {x} b {n} c {obj} d {gone}.", |k| match k {
            "x" => Some(json!("X")),
            "n" => Some(json!(7)),
            "obj" => Some(json!({"k": 1})),
            _ => None,
        });
        assert_eq!(out, "a X b 7 c {\"k\":1} d .");
    }

    #[test]
    fn interpolate_with_trims_keys_and_keeps_an_unclosed_brace() {
        let out = interpolate_with("{ k } and {oops", |k| {
            assert_eq!(k, "k");
            Some(json!("v"))
        });
        assert_eq!(out, "v and {oops");
        assert_eq!(interpolate_with("no braces", |_| None), "no braces");
    }

    #[test]
    fn levenshtein_counts_characters_not_bytes() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("héllo", "hello"), 1);
        assert_eq!(levenshtein("日本語", "日本"), 1);
    }
}
