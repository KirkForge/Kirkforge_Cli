//! Fallback extraction for inline tool-call markup.
//!
//! Some models (notably DeepSeek when routed through an Ollama
//! `/api/chat` proxy) do not emit tool calls in the JSON
//! `message.tool_calls` array. Instead, they stream native DeepSeek
//! Markup Language (DSML) inside `message.content`, e.g.
//!
//! ```text
//! <｜DSML｜invoke name="bash">
//! <｜DSML｜parameter name="command" string="true">ls -la</｜DSML｜parameter>
//! </｜DSML｜invoke>
//! ```
//!
//! This module extracts those blocks, converts them to [`ToolInvocation`]
//! values, and returns the surrounding text with the markup removed so the
//! assistant message stays clean in the conversation log.
//!
//! The parser is intentionally conservative: malformed or unclosed tags
//! are left in the content unchanged rather than silently eaten.

use crate::shared::ToolInvocation;

/// Extract DSML-style tool calls from `content` and return a cleaned
/// version of the text.
///
/// The returned tuple is `(cleaned_content, tool_calls)`. The cleaned
/// content has every fully-parsed `<｜DSML｜invoke>` block removed.
/// Partial/unclosed markup is left as-is so we do not discard model
/// output by accident.
pub fn extract_dsml_tool_calls(content: &str) -> (String, Vec<ToolInvocation>) {
    let mut cleaned = String::with_capacity(content.len());
    let mut calls: Vec<ToolInvocation> = Vec::new();
    let mut cursor = 0usize;

    while cursor < content.len() {
        let Some(tag_start) = find_invoke_open(content, cursor) else {
            break;
        };

        // Append text before this invoke block.
        cleaned.push_str(&content[cursor..tag_start.start]);

        // Extract `name="..."` from the opening tag.
        let Some(tool_name) = tag_start.name else {
            // No usable name — treat the rest as plain text and stop parsing.
            cleaned.push_str(&content[tag_start.start..]);
            return (cleaned, calls);
        };

        // Locate the matching `</｜DSML｜invoke>` close tag.
        let Some(close) = find_invoke_close(content, tag_start.end) else {
            // Unclosed invoke: leave the rest untouched.
            cleaned.push_str(&content[tag_start.start..]);
            return (cleaned, calls);
        };

        let inner = &content[tag_start.end..close.start];
        let args = parse_parameters(inner);

        calls.push(ToolInvocation {
            id: format!("dsml_{}", calls.len()),
            name: tool_name,
            arguments: args,
        });

        cursor = close.end;
    }

    cleaned.push_str(&content[cursor..]);
    (cleaned, calls)
}

/// Position of an opening `<｜DSML｜invoke name="...">` tag.
struct InvokeTag {
    start: usize,
    end: usize,
    name: Option<String>,
}

fn find_invoke_open(content: &str, from: usize) -> Option<InvokeTag> {
    // Iterate over UTF-8 character boundaries so slicing `content[i..]`
    // never panics on multi-byte characters such as em dashes.
    for (offset, _ch) in content[from..].char_indices() {
        let i = from + offset;
        // Try ASCII pipes first, then fullwidth.
        let (tag_start, delim) =
            if content[i..].starts_with("<|DSML|invoke ") {
                (i, "|DSML|")
            } else if content[i..].starts_with("<｜DSML｜invoke ") {
                (i, "｜DSML｜")
            } else {
                continue;
            };

        // The opening tag runs until the next '>'.
        let after_tag = tag_start + "<".len() + delim.len() + "invoke ".len();
        let close = content[after_tag..].find('>')?;
        let tag_end = after_tag + close + 1;

        let name = parse_name_attr(&content[tag_start..tag_end]);
        return Some(InvokeTag {
            start: tag_start,
            end: tag_end,
            name,
        });
    }
    None
}

fn find_invoke_close(content: &str, from: usize) -> Option<InvokeTag> {
    let ascii_close = "</|DSML|invoke>";
    let full_close = "</｜DSML｜invoke>";

    let ascii_pos = content[from..].find(ascii_close).map(|p| from + p);
    let full_pos = content[from..].find(full_close).map(|p| from + p);

    let start = match (ascii_pos, full_pos) {
        (Some(a), Some(f)) => a.min(f),
        (Some(a), None) => a,
        (None, Some(f)) => f,
        (None, None) => return None,
    };

    let len = if content[start..].starts_with(ascii_close) {
        ascii_close.len()
    } else {
        full_close.len()
    };

    Some(InvokeTag {
        start,
        end: start + len,
        name: None,
    })
}

/// Parse `name="..."` from an opening tag.
fn parse_name_attr(tag: &str) -> Option<String> {
    let idx = tag.find("name=")?;
    let rest = &tag[idx + "name=".len()..];
    // The value should be quoted. Support both " and ' for robustness.
    let quote = rest.chars().next()?;
    let end = match quote {
        '"' | '\'' => rest[1..].find(quote)?,
        _ => return None,
    };
    Some(rest[1..=end].to_string())
}

/// Parse `<｜DSML｜parameter name="...">value</｜DSML｜parameter>` blocks.
fn parse_parameters(inner: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let mut cursor = 0usize;

    while let Some(param) = find_parameter_open(inner, cursor) {
        let Some(close) = find_parameter_close(inner, param.end) else {
            break;
        };

        let value = inner[param.end..close.start].trim();
        // Try to parse as JSON; otherwise treat as a string. Tool schemas
        // normally expect strings, but numbers/booleans occasionally leak
        // through in model output.
        let json_value = serde_json::from_str(value)
            .unwrap_or_else(|_| serde_json::Value::String(value.to_string()));

        map.insert(param.name, json_value);
        cursor = close.end;
    }

    serde_json::Value::Object(map)
}

struct ParameterTag {
    start: usize,
    end: usize,
    name: String,
}

fn find_parameter_open(content: &str, from: usize) -> Option<ParameterTag> {
    let ascii_open = "<|DSML|parameter ";
    let full_open = "<｜DSML｜parameter ";

    let ascii_pos = content[from..].find(ascii_open).map(|p| from + p);
    let full_pos = content[from..].find(full_open).map(|p| from + p);

    let start = match (ascii_pos, full_pos) {
        (Some(a), Some(f)) => a.min(f),
        (Some(a), None) => a,
        (None, Some(f)) => f,
        (None, None) => return None,
    };

    let after_tag = if content[start..].starts_with(ascii_open) {
        start + ascii_open.len()
    } else {
        start + full_open.len()
    };

    let close = content[after_tag..].find('>')?;
    let tag_end = after_tag + close + 1;
    let name = parse_name_attr(&content[start..tag_end])?;

    Some(ParameterTag {
        start,
        end: tag_end,
        name,
    })
}

fn find_parameter_close(content: &str, from: usize) -> Option<ParameterTag> {
    let ascii_close = "</|DSML|parameter>";
    let full_close = "</｜DSML｜parameter>";

    let ascii_pos = content[from..].find(ascii_close).map(|p| from + p);
    let full_pos = content[from..].find(full_close).map(|p| from + p);

    let start = match (ascii_pos, full_pos) {
        (Some(a), Some(f)) => a.min(f),
        (Some(a), None) => a,
        (None, Some(f)) => f,
        (None, None) => return None,
    };

    let len = if content[start..].starts_with(ascii_close) {
        ascii_close.len()
    } else {
        full_close.len()
    };

    Some(ParameterTag {
        start,
        end: start + len,
        name: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_single_dsml_tool_call() {
        let content = r#"Looking now.
<｜DSML｜invoke name="bash">
<｜DSML｜parameter name="command" string="true">cd /petsense && git status</｜DSML｜parameter>
</｜DSML｜invoke>
"#;
        let (cleaned, calls) = extract_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments, json!({"command": "cd /petsense && git status"}));
        assert!(!cleaned.contains("DSML"));
        assert!(cleaned.contains("Looking now."));
    }

    #[test]
    fn extracts_multiple_tool_calls() {
        let content = r#"<｜DSML｜invoke name="read_file"><｜DSML｜parameter name="path">/etc/hosts</｜DSML｜parameter></｜DSML｜invoke>
<｜DSML｜invoke name="bash"><｜DSML｜parameter name="command">ls</｜DSML｜parameter></｜DSML｜invoke>"#;
        let (cleaned, calls) = extract_dsml_tool_calls(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, json!({"path": "/etc/hosts"}));
        assert_eq!(calls[1].name, "bash");
        assert_eq!(calls[1].arguments, json!({"command": "ls"}));
        assert!(!cleaned.contains("<｜DSML｜"));
    }

    #[test]
    fn ascii_pipes_also_work() {
        let content = r#"<|DSML|invoke name="bash"><|DSML|parameter name="command">ls</|DSML|parameter></|DSML|invoke>"#;
        let (cleaned, calls) = extract_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert!(cleaned.is_empty());
    }

    #[test]
    fn leaves_plain_text_untouched() {
        let content = "Just a normal response.";
        let (cleaned, calls) = extract_dsml_tool_calls(content);
        assert!(calls.is_empty());
        assert_eq!(cleaned, content);
    }

    #[test]
    fn leaves_unclosed_markup_in_place() {
        let content = "Start <｜DSML｜invoke name=\"bash\"> no close tag";
        let (cleaned, calls) = extract_dsml_tool_calls(content);
        assert!(calls.is_empty());
        assert_eq!(cleaned, content);
    }

    #[test]
    fn numeric_and_boolean_arguments_parsed_as_json() {
        let content = r#"<｜DSML｜invoke name="write_file">
<｜DSML｜parameter name="path">src/main.rs</｜DSML｜parameter>
<｜DSML｜parameter name="append">true</｜DSML｜parameter>
<｜DSML｜parameter name="lines">42</｜DSML｜parameter>
</｜DSML｜invoke>"#;
        let (_cleaned, calls) = extract_dsml_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].arguments,
            json!({"path": "src/main.rs", "append": true, "lines": 42})
        );
    }
}
