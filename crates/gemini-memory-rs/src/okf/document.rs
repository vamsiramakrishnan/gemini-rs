//! The OKF document container: YAML front matter plus Markdown body sections.
//!
//! A memory record is a Markdown file a human can read, diff and hand-edit.
//! The front matter carries machine state; the body carries the sentences the
//! model will actually see.

use std::collections::BTreeMap;

use super::yaml::{self, Yaml};
use crate::core::MemoryError;

/// The front-matter delimiter.
const FENCE: &str = "---";

/// A parsed OKF file.
#[derive(Debug, Clone, PartialEq)]
pub struct OkfDocument {
    /// Machine-readable front matter.
    pub front: Yaml,
    /// Body sections, keyed by their `#` heading text.
    pub sections: BTreeMap<String, String>,
    /// Heading order as it appeared, so emitting is stable.
    pub section_order: Vec<String>,
}

impl OkfDocument {
    /// Build a document from front matter and ordered sections.
    pub fn new(front: Yaml, sections: Vec<(&str, String)>) -> Self {
        let mut order = Vec::new();
        let mut map = BTreeMap::new();
        for (heading, body) in sections {
            order.push(heading.to_string());
            map.insert(heading.to_string(), body);
        }
        Self {
            front,
            sections: map,
            section_order: order,
        }
    }

    /// Read a section body by heading.
    pub fn section(&self, heading: &str) -> Option<&str> {
        self.sections.get(heading).map(|s| s.trim())
    }

    /// Read a section as a `- item` list.
    pub fn section_list(&self, heading: &str) -> Vec<String> {
        self.section(heading)
            .map(|body| {
                body.lines()
                    .filter_map(|line| line.trim().strip_prefix("- ").map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Parse an OKF file.
    pub fn parse(source: &str, path: &str) -> Result<Self, MemoryError> {
        let source = source.trim_start_matches('\u{feff}');
        let rest = source
            .strip_prefix(FENCE)
            .and_then(|r| r.strip_prefix('\n').or_else(|| r.strip_prefix("\r\n")))
            .ok_or_else(|| MemoryError::MalformedRecord {
                path: path.to_string(),
                message: "file does not begin with a `---` front-matter fence".to_string(),
            })?;

        let end = rest
            .find("\n---")
            .ok_or_else(|| MemoryError::MalformedRecord {
                path: path.to_string(),
                message: "front matter is not terminated by `---`".to_string(),
            })?;

        let front_src = &rest[..end];
        let body = rest[end + 4..].trim_start_matches(['\r', '\n']);

        let front = yaml::parse(front_src).map_err(|e| MemoryError::MalformedRecord {
            path: path.to_string(),
            message: e.to_string(),
        })?;

        let (sections, section_order) = parse_sections(body);
        Ok(Self {
            front,
            sections,
            section_order,
        })
    }

    /// Parse a category file holding several records back to back.
    ///
    /// Records are delimited by `---` fences in pairs: open, front matter,
    /// close, body, then the next record's open fence. A body line that is
    /// exactly `---` would therefore be read as a record boundary, so
    /// [`Self::render_many`] refuses to write one.
    pub fn parse_many(source: &str, path: &str) -> Result<Vec<Self>, MemoryError> {
        let source = source.trim_start_matches('\u{feff}');
        let lines: Vec<&str> = source.lines().collect();
        let fences: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim_end() == FENCE)
            .map(|(i, _)| i)
            .collect();

        if fences.is_empty() {
            return if source.trim().is_empty() {
                Ok(Vec::new())
            } else {
                Err(MemoryError::MalformedRecord {
                    path: path.to_string(),
                    message: "file contains no `---` front-matter fence".to_string(),
                })
            };
        }
        if !fences.len().is_multiple_of(2) {
            return Err(MemoryError::MalformedRecord {
                path: path.to_string(),
                message: format!(
                    "odd number of `---` fences ({}) — a record is unterminated",
                    fences.len()
                ),
            });
        }

        let mut docs = Vec::new();
        for pair in 0..fences.len() / 2 {
            let open = fences[pair * 2];
            let close = fences[pair * 2 + 1];
            let body_end = fences.get((pair + 1) * 2).copied().unwrap_or(lines.len());
            let mut chunk = String::new();
            for line in &lines[open..close.min(body_end)] {
                chunk.push_str(line);
                chunk.push('\n');
            }
            chunk.push_str(FENCE);
            chunk.push('\n');
            for line in &lines[close + 1..body_end] {
                chunk.push_str(line);
                chunk.push('\n');
            }
            docs.push(Self::parse(&chunk, path)?);
        }
        Ok(docs)
    }

    /// Render several records into one category file.
    pub fn render_many(docs: &[Self], path: &str) -> Result<String, MemoryError> {
        let mut out = String::new();
        for (idx, doc) in docs.iter().enumerate() {
            if doc
                .sections
                .values()
                .any(|body| body.lines().any(|l| l.trim_end() == FENCE))
            {
                return Err(MemoryError::MalformedRecord {
                    path: path.to_string(),
                    message: "record body contains a `---` line, which would be read as a \
                              record boundary"
                        .to_string(),
                });
            }
            if idx > 0 {
                out.push('\n');
            }
            out.push_str(&doc.to_markdown());
        }
        Ok(out)
    }

    /// Render the document back to text.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(FENCE);
        out.push('\n');
        out.push_str(&yaml::emit(&self.front));
        out.push_str(FENCE);
        out.push('\n');
        for heading in &self.section_order {
            if let Some(body) = self.sections.get(heading) {
                out.push_str("\n# ");
                out.push_str(heading);
                out.push('\n');
                let trimmed = body.trim();
                if !trimmed.is_empty() {
                    out.push_str(trimmed);
                    out.push('\n');
                }
            }
        }
        out
    }
}

fn parse_sections(body: &str) -> (BTreeMap<String, String>, Vec<String>) {
    let mut sections = BTreeMap::new();
    let mut order = Vec::new();
    let mut current: Option<String> = None;
    let mut buffer = String::new();

    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("# ") {
            if let Some(name) = current.take() {
                sections.insert(name, buffer.trim().to_string());
            }
            buffer = String::new();
            let heading = heading.trim().to_string();
            order.push(heading.clone());
            current = Some(heading);
        } else if current.is_some() {
            buffer.push_str(line);
            buffer.push('\n');
        }
    }
    if let Some(name) = current {
        sections.insert(name, buffer.trim().to_string());
    }
    (sections, order)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
okf: memory/v1
id: mem_01K4D8P3
confidence: 1.0
retrieval:
  tags:
    - food
    - diet
---
# Fact
The user is pescatarian.

# Evidence Summary
Explicitly stated by the user.

# Supersedes
- mem_01JVEGETARIAN
"#;

    #[test]
    fn parses_front_matter_and_sections() {
        let doc = OkfDocument::parse(SAMPLE, "sample.md").unwrap();
        assert_eq!(doc.front.get("id").unwrap().as_str(), Some("mem_01K4D8P3"));
        assert_eq!(doc.section("Fact"), Some("The user is pescatarian."));
        assert_eq!(doc.section_list("Supersedes"), vec!["mem_01JVEGETARIAN"]);
    }

    #[test]
    fn round_trips_to_markdown() {
        let doc = OkfDocument::parse(SAMPLE, "sample.md").unwrap();
        let rendered = doc.to_markdown();
        let reparsed = OkfDocument::parse(&rendered, "rendered.md").unwrap();
        assert_eq!(doc.front, reparsed.front);
        assert_eq!(doc.sections, reparsed.sections);
    }

    #[test]
    fn a_missing_fence_names_the_file() {
        let err = OkfDocument::parse("no front matter", "records/x.md").unwrap_err();
        match err {
            MemoryError::MalformedRecord { path, .. } => assert_eq!(path, "records/x.md"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn an_unterminated_fence_is_rejected() {
        assert!(OkfDocument::parse("---\nid: x\n", "x.md").is_err());
    }

    #[test]
    fn category_files_hold_several_records() {
        let first = OkfDocument::parse(SAMPLE, "s.md").unwrap();
        let second = OkfDocument::new(
            yaml::parse("okf: memory/v1\nid: mem_two\n").unwrap(),
            vec![("Fact", "The user drinks flat whites.".to_string())],
        );
        let file = OkfDocument::render_many(&[first.clone(), second.clone()], "cat.md").unwrap();

        let parsed = OkfDocument::parse_many(&file, "cat.md").unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].front, first.front);
        assert_eq!(parsed[1].section("Fact"), second.section("Fact"));
    }

    #[test]
    fn an_empty_category_file_parses_to_no_records() {
        assert!(OkfDocument::parse_many("", "cat.md").unwrap().is_empty());
        assert!(
            OkfDocument::parse_many("\n\n", "cat.md")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_body_containing_a_fence_is_refused_at_write_time() {
        let doc = OkfDocument::new(
            yaml::parse("okf: memory/v1\n").unwrap(),
            vec![("Fact", "before\n---\nafter".to_string())],
        );
        assert!(OkfDocument::render_many(&[doc], "cat.md").is_err());
    }

    #[test]
    fn an_odd_fence_count_names_the_problem() {
        let err =
            OkfDocument::parse_many("---\nid: a\n---\n# Fact\nx\n---\n", "cat.md").unwrap_err();
        assert!(err.to_string().contains("unterminated"));
    }
}
