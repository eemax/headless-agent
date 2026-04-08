use std::{collections::HashSet, io::Write};

use serde_json::Value;

use crate::{error::AppError, types::ToolCallRecord};

const CLIP_LIMIT: usize = 200;

pub trait StderrSink {
    fn emit_line(&mut self, line: &str) -> Result<(), AppError>;
}

#[derive(Debug, Default)]
pub struct BufferedStderrSink {
    lines: Vec<String>,
}

impl BufferedStderrSink {
    pub fn into_lines(self) -> Vec<String> {
        self.lines
    }
}

impl StderrSink for BufferedStderrSink {
    fn emit_line(&mut self, line: &str) -> Result<(), AppError> {
        self.lines.push(line.to_string());
        Ok(())
    }
}

pub struct WriterStderrSink<'a, W: Write> {
    writer: &'a mut W,
}

impl<'a, W: Write> WriterStderrSink<'a, W> {
    pub fn new(writer: &'a mut W) -> Self {
        Self { writer }
    }
}

impl<W: Write> StderrSink for WriterStderrSink<'_, W> {
    fn emit_line(&mut self, line: &str) -> Result<(), AppError> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

pub struct ProgressReporter<'a> {
    verbose: bool,
    sink: &'a mut dyn StderrSink,
}

impl<'a> ProgressReporter<'a> {
    pub fn new(verbose: bool, sink: &'a mut dyn StderrSink) -> Self {
        Self { verbose, sink }
    }

    pub fn emit_line(&mut self, line: impl AsRef<str>) -> Result<(), AppError> {
        self.sink.emit_line(line.as_ref())
    }

    pub fn emit_provider_step(
        &mut self,
        step: usize,
        reasoning: Option<&Value>,
        reasoning_details: Option<&Value>,
        tool_calls: &[ToolCallRecord],
    ) -> Result<(), AppError> {
        if !self.verbose {
            return Ok(());
        }

        let step_number = step + 1;
        if let Some(text) = extract_reasoning_text(reasoning, reasoning_details) {
            self.emit_line(format!(
                "step {step_number} reasoning: {}",
                clip_text(&text, CLIP_LIMIT)
            ))?;
        }

        let total = tool_calls.len();
        for (index, tool_call) in tool_calls.iter().enumerate() {
            self.emit_line(format!(
                "step {step_number} tool {}/{}: {}",
                index + 1,
                total,
                summarize_tool_call(tool_call)
            ))?;
        }

        Ok(())
    }
}

fn extract_reasoning_text(
    reasoning: Option<&Value>,
    reasoning_details: Option<&Value>,
) -> Option<String> {
    if let Some(details) = reasoning_details.and_then(Value::as_array) {
        let joined = details
            .iter()
            .filter_map(|entry| entry.get("text").and_then(Value::as_str))
            .map(normalize_for_stderr)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.is_empty() {
            return Some(joined);
        }
    }

    match reasoning {
        Some(Value::String(text)) => {
            let normalized = normalize_for_stderr(text);
            (!normalized.is_empty()).then_some(normalized)
        }
        Some(Value::Object(map)) => map
            .get("text")
            .and_then(Value::as_str)
            .map(normalize_for_stderr)
            .filter(|text| !text.is_empty()),
        _ => None,
    }
}

fn summarize_tool_call(tool_call: &ToolCallRecord) -> String {
    let arguments = &tool_call.arguments;
    let summary = match tool_call.name.as_str() {
        "bash" => {
            if let Some(command) = arguments.get("command").and_then(Value::as_str) {
                format!("bash {}", normalize_for_stderr(command))
            } else {
                "bash".to_string()
            }
        }
        "write_file" | "edit_file" => {
            if let Some(path) = arguments.get("path").and_then(Value::as_str) {
                format!(
                    r#"{} path="{}""#,
                    tool_call.name,
                    normalize_for_stderr(path)
                )
            } else {
                tool_call.name.clone()
            }
        }
        "apply_patch" => summarize_apply_patch(arguments),
        _ => match serde_json::to_string(arguments) {
            Ok(json) => format!("{} {}", tool_call.name, json),
            Err(_) => tool_call.name.clone(),
        },
    };
    clip_text(&summary, CLIP_LIMIT)
}

fn summarize_apply_patch(arguments: &Value) -> String {
    let Some(patch) = arguments.get("patch").and_then(Value::as_str) else {
        return "apply_patch".to_string();
    };
    let paths = extract_patch_paths(patch);
    if paths.is_empty() {
        "apply_patch".to_string()
    } else {
        format!("apply_patch {}", paths.join(", "))
    }
}

fn extract_patch_paths(patch: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for line in patch.lines() {
        let path = line
            .strip_prefix("*** Add File: ")
            .or_else(|| line.strip_prefix("*** Delete File: "))
            .or_else(|| line.strip_prefix("*** Update File: "))
            .or_else(|| line.strip_prefix("*** Move to: "));
        if let Some(path) = path {
            let normalized = normalize_for_stderr(path);
            if !normalized.is_empty() && seen.insert(normalized.clone()) {
                paths.push(normalized);
            }
        }
    }
    paths
}

fn clip_text(text: &str, limit: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= limit {
        return text.to_string();
    }

    let clipped: String = text.chars().take(limit).collect();
    format!("{clipped}...[+{} chars]", char_count - limit)
}

fn normalize_for_stderr(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{clip_text, extract_patch_paths, summarize_apply_patch};

    #[test]
    fn clip_text_uses_requested_suffix() {
        let clipped = clip_text("abcdefgh", 5);
        assert_eq!(clipped, "abcde...[+3 chars]");
    }

    #[test]
    fn apply_patch_path_extraction_finds_changed_files() {
        let patch = "\
*** Begin Patch
*** Update File: src/app.rs
@@
-old
+new
*** Move to: src/new_app.rs
*** Add File: README.md
+hello
*** Delete File: docs/old.md
*** End Patch";
        let paths = extract_patch_paths(patch);
        assert_eq!(
            paths,
            vec![
                "src/app.rs".to_string(),
                "src/new_app.rs".to_string(),
                "README.md".to_string(),
                "docs/old.md".to_string()
            ]
        );
    }

    #[test]
    fn apply_patch_summary_omits_patch_body() {
        let summary = summarize_apply_patch(&json!({
            "patch": "\
*** Begin Patch
*** Update File: src/app.rs
@@
-secret body
+new body
*** End Patch"
        }));
        assert_eq!(summary, "apply_patch src/app.rs");
        assert!(!summary.contains("secret body"));
    }
}
