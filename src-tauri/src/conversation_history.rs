use std::fs;
use std::path::Path;

use crate::token_usage;

#[derive(Debug, Clone, PartialEq)]
pub enum ConversationEntry {
    User {
        content: String,
        timestamp: String,
    },
    Assistant {
        content: String,
        timestamp: String,
    },
    ToolUse {
        tool: String,
        input_summary: String,
    },
    ToolResult {
        content: String,
    },
}

/// Load conversation history for a Claude Code session by finding the most
/// recent JSONL file in the project directory derived from `working_dir`.
pub fn load_conversation_history(working_dir: &str) -> Result<Vec<ConversationEntry>, String> {
    let project_dir = token_usage::claude_project_dir(working_dir)?;
    let jsonl_path = token_usage::most_recent_jsonl(&project_dir)?;
    parse_conversation_jsonl(&jsonl_path)
}

/// Parse a Claude Code JSONL file into conversation entries.
pub fn parse_conversation_jsonl(path: &Path) -> Result<Vec<ConversationEntry>, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();

    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let entry_type = match v.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => continue,
        };

        let timestamp = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        match entry_type {
            "user" => {
                if let Some(text) = extract_user_content(&v) {
                    if !text.is_empty() {
                        entries.push(ConversationEntry::User {
                            content: text,
                            timestamp,
                        });
                    }
                }
            }
            "assistant" => {
                let mut assistant_entries = extract_assistant_content(&v, &timestamp);
                entries.append(&mut assistant_entries);
            }
            _ => {}
        }
    }

    Ok(entries)
}

/// Extract text content from a user message.
/// `message.content` can be a string or an array of content blocks.
fn extract_user_content(v: &serde_json::Value) -> Option<String> {
    let content = v.pointer("/message/content")?;

    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }

    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for block in arr {
            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if block_type == "text" {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    parts.push(text.to_string());
                }
            }
            // Skip tool_result and image blocks
        }
        if parts.is_empty() {
            return None;
        }
        return Some(parts.join("\n"));
    }

    None
}

/// Extract content blocks from an assistant message.
/// Returns text entries and tool_use entries. Skips thinking blocks.
fn extract_assistant_content(v: &serde_json::Value, timestamp: &str) -> Vec<ConversationEntry> {
    let mut entries = Vec::new();

    let content = match v.pointer("/message/content") {
        Some(c) => c,
        None => return entries,
    };

    let arr = match content.as_array() {
        Some(a) => a,
        None => return entries,
    };

    let mut text_parts = Vec::new();

    for block in arr {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            "tool_use" => {
                // Flush any accumulated text before the tool_use
                if !text_parts.is_empty() {
                    entries.push(ConversationEntry::Assistant {
                        content: text_parts.join("\n"),
                        timestamp: timestamp.to_string(),
                    });
                    text_parts.clear();
                }

                let tool_name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let input_summary = summarize_tool_input(block.get("input"));

                entries.push(ConversationEntry::ToolUse {
                    tool: tool_name,
                    input_summary,
                });
            }
            "thinking" => {
                // Skip thinking blocks entirely
            }
            _ => {}
        }
    }

    // Flush remaining text
    if !text_parts.is_empty() {
        entries.push(ConversationEntry::Assistant {
            content: text_parts.join("\n"),
            timestamp: timestamp.to_string(),
        });
    }

    entries
}

/// Create a short summary of tool input for display.
fn summarize_tool_input(input: Option<&serde_json::Value>) -> String {
    let input = match input {
        Some(v) => v,
        None => return String::new(),
    };

    // For objects, try to show meaningful fields concisely
    if let Some(obj) = input.as_object() {
        let mut parts = Vec::new();
        for (key, val) in obj {
            let val_str = match val {
                serde_json::Value::String(s) => {
                    if s.len() > 80 {
                        format!("{}...", &s[..77])
                    } else {
                        s.clone()
                    }
                }
                other => {
                    let s = other.to_string();
                    if s.len() > 80 {
                        format!("{}...", &s[..77])
                    } else {
                        s
                    }
                }
            };
            parts.push(format!("{key}: {val_str}"));
        }
        return parts.join(", ");
    }

    input.to_string()
}

/// Render conversation entries as ANSI-formatted text suitable for xterm.js.
/// Uses `\r\n` line endings (terminal convention) and word-wraps at `cols` width.
pub fn render_history_as_terminal(entries: &[ConversationEntry], cols: u16) -> String {
    let mut output = String::new();

    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            output.push_str("\r\n");
        }

        match entry {
            ConversationEntry::User { content, .. } => {
                output.push_str("\x1b[1;34m\u{276F} You\x1b[0m\r\n");
                output.push_str(&word_wrap_terminal(content, cols));
            }
            ConversationEntry::Assistant { content, .. } => {
                output.push_str("\x1b[1;32m\u{276F} Claude\x1b[0m\r\n");
                output.push_str(&word_wrap_terminal(content, cols));
            }
            ConversationEntry::ToolUse {
                tool,
                input_summary,
            } => {
                let line = format!("  \u{27E1} {tool}: {input_summary}");
                let truncated = if line.len() > 120 {
                    format!("{}...", &line[..117])
                } else {
                    line
                };
                output.push_str(&format!("\x1b[2m{truncated}\x1b[0m\r\n"));
            }
            ConversationEntry::ToolResult { .. } => {
                // Skip — too verbose
            }
        }
    }

    output
}

/// Word-wrap text at `cols` width, using `\r\n` line endings.
fn word_wrap_terminal(text: &str, cols: u16) -> String {
    let cols = cols as usize;
    let mut output = String::new();

    for line in text.split('\n') {
        if line.is_empty() {
            output.push_str("\r\n");
            continue;
        }

        let mut current_len = 0;
        let mut first_word = true;

        for word in line.split_whitespace() {
            let word_len = word.len();

            if !first_word && current_len + 1 + word_len > cols {
                // Wrap to next line
                output.push_str("\r\n");
                current_len = 0;
                first_word = true;
            }

            if !first_word {
                output.push(' ');
                current_len += 1;
            }

            // Handle words longer than cols
            if word_len > cols {
                let mut remaining = word;
                while !remaining.is_empty() {
                    let take = if current_len == 0 {
                        cols
                    } else {
                        cols - current_len
                    };
                    let take = take.min(remaining.len());
                    output.push_str(&remaining[..take]);
                    remaining = &remaining[take..];
                    current_len += take;
                    if !remaining.is_empty() {
                        output.push_str("\r\n");
                        current_len = 0;
                    }
                }
            } else {
                output.push_str(word);
                current_len += word_len;
            }

            first_word = false;
        }

        output.push_str("\r\n");
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_jsonl(dir: &TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("test.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        write!(f, "{}", content).unwrap();
        path
    }

    #[test]
    fn test_parse_user_string_content() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Hello world"}}"#,
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ConversationEntry::User { content, timestamp } => {
                assert_eq!(content, "Hello world");
                assert_eq!(timestamp, "2026-01-01T00:00:00Z");
            }
            _ => panic!("Expected User entry"),
        }
    }

    #[test]
    fn test_parse_user_array_content() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"Hello"},{"type":"text","text":"World"}]}}"#,
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ConversationEntry::User { content, .. } => {
                assert_eq!(content, "Hello\nWorld");
            }
            _ => panic!("Expected User entry"),
        }
    }

    #[test]
    fn test_parse_user_skips_tool_result_blocks() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"tool_result","content":"result"},{"type":"text","text":"Follow up"}]}}"#,
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ConversationEntry::User { content, .. } => {
                assert_eq!(content, "Follow up");
            }
            _ => panic!("Expected User entry"),
        }
    }

    #[test]
    fn test_parse_assistant_text_blocks() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:01:00Z","message":{"content":[{"type":"text","text":"I will help you."},{"type":"text","text":"Here is the plan."}]}}"#,
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ConversationEntry::Assistant { content, .. } => {
                assert_eq!(content, "I will help you.\nHere is the plan.");
            }
            _ => panic!("Expected Assistant entry"),
        }
    }

    #[test]
    fn test_parse_assistant_tool_use() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:01:00Z","message":{"content":[{"type":"text","text":"Let me read the file."},{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/test.rs"}},{"type":"text","text":"Done."}]}}"#,
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 3);
        match &entries[0] {
            ConversationEntry::Assistant { content, .. } => {
                assert_eq!(content, "Let me read the file.");
            }
            _ => panic!("Expected Assistant entry"),
        }
        match &entries[1] {
            ConversationEntry::ToolUse {
                tool,
                input_summary,
            } => {
                assert_eq!(tool, "Read");
                assert!(input_summary.contains("file_path"));
                assert!(input_summary.contains("/tmp/test.rs"));
            }
            _ => panic!("Expected ToolUse entry"),
        }
        match &entries[2] {
            ConversationEntry::Assistant { content, .. } => {
                assert_eq!(content, "Done.");
            }
            _ => panic!("Expected Assistant entry"),
        }
    }

    #[test]
    fn test_parse_skips_thinking_blocks() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:01:00Z","message":{"content":[{"type":"thinking","thinking":"internal thought"},{"type":"text","text":"Visible response."}]}}"#,
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ConversationEntry::Assistant { content, .. } => {
                assert_eq!(content, "Visible response.");
            }
            _ => panic!("Expected Assistant entry"),
        }
    }

    #[test]
    fn test_parse_skips_non_user_assistant_types() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            &[
                r#"{"type":"progress","timestamp":"2026-01-01T00:00:00Z"}"#,
                r#"{"type":"system","timestamp":"2026-01-01T00:00:00Z","message":{"content":"sys"}}"#,
                r#"{"type":"file-history-snapshot","timestamp":"2026-01-01T00:00:00Z"}"#,
                r#"{"type":"user","timestamp":"2026-01-01T00:00:01Z","message":{"content":"Hi"}}"#,
            ]
            .join("\n"),
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(&dir, "");
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_malformed_lines_skipped() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            &[
                "not valid json",
                r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":"Ok"}}"#,
            ]
            .join("\n"),
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_render_user_message() {
        let entries = vec![ConversationEntry::User {
            content: "Hello".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }];
        let output = render_history_as_terminal(&entries, 80);
        assert!(output.contains("\x1b[1;34m\u{276F} You\x1b[0m"));
        assert!(output.contains("Hello"));
    }

    #[test]
    fn test_render_assistant_message() {
        let entries = vec![ConversationEntry::Assistant {
            content: "I can help.".to_string(),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }];
        let output = render_history_as_terminal(&entries, 80);
        assert!(output.contains("\x1b[1;32m\u{276F} Claude\x1b[0m"));
        assert!(output.contains("I can help."));
    }

    #[test]
    fn test_render_tool_use() {
        let entries = vec![ConversationEntry::ToolUse {
            tool: "Read".to_string(),
            input_summary: "file_path: /tmp/test.rs".to_string(),
        }];
        let output = render_history_as_terminal(&entries, 80);
        assert!(output.contains("\x1b[2m"));
        assert!(output.contains("Read"));
        assert!(output.contains("/tmp/test.rs"));
    }

    #[test]
    fn test_render_tool_use_truncation() {
        let entries = vec![ConversationEntry::ToolUse {
            tool: "Write".to_string(),
            input_summary: "a".repeat(200),
        }];
        let output = render_history_as_terminal(&entries, 80);
        assert!(output.contains("..."));
        // The dim escape + content + reset should be bounded
    }

    #[test]
    fn test_render_skips_tool_result() {
        let entries = vec![ConversationEntry::ToolResult {
            content: "Should not appear".to_string(),
        }];
        let output = render_history_as_terminal(&entries, 80);
        assert!(!output.contains("Should not appear"));
    }

    #[test]
    fn test_word_wrap() {
        let text = "This is a line that should be wrapped at a short column width";
        let wrapped = word_wrap_terminal(text, 20);
        // All lines should end with \r\n
        for line in wrapped.split("\r\n") {
            if !line.is_empty() {
                assert!(line.len() <= 20, "Line too long: '{}' ({})", line, line.len());
            }
        }
    }

    #[test]
    fn test_word_wrap_preserves_empty_lines() {
        let text = "Line 1\n\nLine 3";
        let wrapped = word_wrap_terminal(text, 80);
        // Should contain an empty line (just \r\n\r\n)
        assert!(wrapped.contains("\r\n\r\n"));
    }

    #[test]
    fn test_word_wrap_long_word() {
        let long_word = "a".repeat(30);
        let wrapped = word_wrap_terminal(&long_word, 10);
        for line in wrapped.split("\r\n") {
            if !line.is_empty() {
                assert!(line.len() <= 10);
            }
        }
    }

    #[test]
    fn test_render_full_conversation() {
        let entries = vec![
            ConversationEntry::User {
                content: "Fix the bug in main.rs".to_string(),
                timestamp: "2026-01-01T00:00:00Z".to_string(),
            },
            ConversationEntry::Assistant {
                content: "I will look at the file.".to_string(),
                timestamp: "2026-01-01T00:00:01Z".to_string(),
            },
            ConversationEntry::ToolUse {
                tool: "Read".to_string(),
                input_summary: "file_path: /src/main.rs".to_string(),
            },
            ConversationEntry::ToolResult {
                content: "fn main() {}".to_string(),
            },
            ConversationEntry::Assistant {
                content: "Fixed it.".to_string(),
                timestamp: "2026-01-01T00:00:02Z".to_string(),
            },
        ];
        let output = render_history_as_terminal(&entries, 120);
        // Should have user header, assistant header x2, tool use, no tool result
        assert!(output.contains("You"));
        assert!(output.contains("Claude"));
        assert!(output.contains("Read"));
        assert!(!output.contains("fn main()"));
        // Lines use \r\n
        assert!(output.contains("\r\n"));
        assert!(!output.contains("\n\n")); // No bare \n\n
    }

    #[test]
    fn test_summarize_tool_input_truncates() {
        let long_val = serde_json::json!({"content": "x".repeat(200)});
        let summary = summarize_tool_input(Some(&long_val));
        // The individual value should be truncated
        assert!(summary.len() < 200);
    }

    #[test]
    fn test_user_only_tool_result_blocks_produces_no_entry() {
        let dir = TempDir::new().unwrap();
        let path = make_jsonl(
            &dir,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"tool_result","content":"result"}]}}"#,
        );
        let entries = parse_conversation_jsonl(&path).unwrap();
        assert!(entries.is_empty());
    }
}
