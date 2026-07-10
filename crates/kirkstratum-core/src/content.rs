use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Kind of content the pipeline is processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentType {
    /// A JSON array payload (`[...]`).
    JsonArray,
    /// A JSON object payload (`{...}`).
    JsonObject,
    /// Source code (Rust, Python, JS, C, etc.).
    SourceCode,
    /// Grep/find style `path:line:match` results.
    SearchResults,
    /// Compiler or test runner output.
    BuildOutput,
    /// A unified `diff` patch.
    GitDiff,
    /// HTML or XML-like markup.
    Html,
    /// Plain text fallback.
    PlainText,
}

impl ContentType {
    /// All supported content types in declaration order.
    pub const ALL: [Self; 8] = [
        Self::JsonArray,
        Self::JsonObject,
        Self::SourceCode,
        Self::SearchResults,
        Self::BuildOutput,
        Self::GitDiff,
        Self::Html,
        Self::PlainText,
    ];

    /// Return the `snake_case` name used in config and CLI values.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JsonArray => "json_array",
            Self::JsonObject => "json_object",
            Self::SourceCode => "source_code",
            Self::SearchResults => "search_results",
            Self::BuildOutput => "build_output",
            Self::GitDiff => "git_diff",
            Self::Html => "html",
            Self::PlainText => "plain_text",
        }
    }

    /// Return a human-readable label for this content type.
    ///
    /// This is the `snake_case` name with underscores replaced by spaces, kept
    /// stable so report consumers can rely on it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::JsonArray => "json array",
            Self::JsonObject => "json object",
            Self::SourceCode => "source code",
            Self::SearchResults => "search results",
            Self::BuildOutput => "build output",
            Self::GitDiff => "git diff",
            Self::Html => "html",
            Self::PlainText => "plain text",
        }
    }
}

impl fmt::Display for ContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a content type string does not match a known variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown content type: {value}; expected one of: {}", ContentType::ALL.iter().map(|ct| ct.as_str()).collect::<Vec<_>>().join(", "))]
pub struct ContentTypeParseError {
    value: String,
}

impl ContentTypeParseError {
    /// Create a [`ContentTypeParseError`] for the unknown value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    /// The unknown content type value that failed to parse.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl FromStr for ContentType {
    type Err = ContentTypeParseError;

    /// Parse a content type from its `snake_case` string.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::content::ContentType;
    ///
    /// let content_type: ContentType = "git_diff".parse().unwrap();
    /// assert_eq!(content_type, ContentType::GitDiff);
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "json_array" => Ok(Self::JsonArray),
            "json_object" => Ok(Self::JsonObject),
            "source_code" => Ok(Self::SourceCode),
            "search_results" => Ok(Self::SearchResults),
            "build_output" => Ok(Self::BuildOutput),
            "git_diff" => Ok(Self::GitDiff),
            "html" => Ok(Self::Html),
            "plain_text" => Ok(Self::PlainText),
            _ => Err(ContentTypeParseError::new(normalized)),
        }
    }
}

/// Detect the most likely content type from a payload.
///
/// Detection is layered and conservative: a strong signal wins; otherwise it
/// falls back to `PlainText`.
#[must_use]
pub fn detect_content_type(content: &str) -> ContentType {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return ContentType::PlainText;
    }

    // JSON array or object.
    if trimmed.starts_with('[') {
        return ContentType::JsonArray;
    }
    if trimmed.starts_with('{') {
        return ContentType::JsonObject;
    }

    // Git diff: unified diff hunk headers are a strong signal.
    if trimmed.lines().any(|line| line.starts_with("@@ ")) {
        return ContentType::GitDiff;
    }

    // HTML: starts with a doctype or tag-like structure.
    let first_line = trimmed.lines().next().unwrap_or(trimmed);
    if first_line.starts_with("<!DOCTYPE")
        || first_line.starts_with("<!doctype")
        || trimmed.starts_with('<')
    {
        return ContentType::Html;
    }

    // Build output: lines that look like compiler/test errors.
    if trimmed.lines().any(|line| {
        ["error[", "error:", "warning:", "test result:"]
            .iter()
            .any(|marker| line.starts_with(marker))
    }) {
        return ContentType::BuildOutput;
    }

    // Search results: lines that look like grep/find output (path:line:match).
    if trimmed.lines().any(|line| {
        line.split_once(':').is_some_and(|(path, rest)| {
            !path.is_empty()
                && path.contains('/')
                && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
        })
    }) {
        return ContentType::SearchResults;
    }

    // Source code: significant indentation or common structural tokens.
    let structural_score = trimmed
        .lines()
        .take(20)
        .filter(|line| {
            let trimmed_line = line.trim_start();
            [
                "fn ",
                "pub ",
                "struct ",
                "class ",
                "def ",
                "import ",
                "#include",
                "function ",
            ]
            .iter()
            .any(|token| trimmed_line.starts_with(token))
        })
        .count();
    if structural_score >= 2 {
        return ContentType::SourceCode;
    }

    ContentType::PlainText
}

/// Filter Obsidian-style tags by host.
///
/// Rules:
/// - `#tag` is active for every host.
/// - `#tag/host` is active only for the matching host.
/// - A tag that does not match the supplied host is removed (deactivated).
#[must_use]
pub fn filter_deactivated_tags(content: &str, host: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut last_end = 0;

    for (start, _) in content.match_indices('#') {
        // Copy everything up to this tag.
        out.push_str(&content[last_end..start]);

        // Find the end of the tag (whitespace terminates a tag).
        let after_hash = start + 1;
        let tag_end = content[after_hash..]
            .find(char::is_whitespace)
            .map_or(content.len(), |i| after_hash + i);

        let tag = &content[start..tag_end];
        if is_tag_active(tag, host) {
            out.push_str(tag);
        }

        last_end = tag_end;
    }

    out.push_str(&content[last_end..]);
    out
}

fn is_tag_active(tag: &str, host: &str) -> bool {
    // Strip leading '#'.
    let body = tag.strip_prefix('#').unwrap_or(tag);
    if body.is_empty() {
        return true;
    }

    // Split on the first '/' to detect host-scoped tags.
    if let Some((base, tag_host)) = body.split_once('/') {
        let _ = base; // base name does not affect activation logic
        tag_host.eq_ignore_ascii_case(host)
    } else {
        true
    }
}

/// Remove sections delimited by `<!-- private -->` and `<!-- /private -->`.
#[must_use]
pub fn remove_private_sections(content: &str) -> String {
    remove_delimited_sections(content, "private")
}

/// Remove sections delimited by `<!-- public -->` and `<!-- /public -->`.
#[must_use]
pub fn remove_public_sections(content: &str) -> String {
    remove_delimited_sections(content, "public")
}

fn remove_delimited_sections(content: &str, marker: &str) -> String {
    let start_marker = format!("<!-- {marker} -->");
    let end_marker = format!("<!-- /{marker} -->");

    let mut out = String::with_capacity(content.len());
    let mut rest = content;

    while let Some(start) = rest.find(&start_marker) {
        out.push_str(&rest[..start]);
        let after_start = start + start_marker.len();
        if let Some(end) = rest[after_start..].find(&end_marker) {
            rest = &rest[after_start + end + end_marker.len()..];
        } else {
            // No closing marker: drop the rest. This is the safe default for
            // removing private/public content; leaving everything in place
            // would risk leaking material that was explicitly marked private.
            rest = "";
            break;
        }
    }

    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_deactivation_by_host() {
        let input = "#note #note/claude #note/codex";
        let out = filter_deactivated_tags(input, "claude");
        assert!(out.contains("#note"));
        assert!(out.contains("#note/claude"));
        assert!(!out.contains("#note/codex"));
    }

    #[test]
    fn publish_mode_removes_private_sections() {
        let input = "Intro\n<!-- private -->\nsecret\n<!-- /private -->\nOutro";
        let out = remove_private_sections(input);
        assert!(out.contains("Intro"));
        assert!(!out.contains("secret"));
        assert!(out.contains("Outro"));
    }

    #[test]
    fn standalone_mode_removes_public_sections() {
        let input = "Intro\n<!-- public -->\nshared context\n<!-- /public -->\nBody";
        let out = remove_public_sections(input);
        assert!(out.contains("Intro"));
        assert!(!out.contains("shared context"));
        assert!(out.contains("Body"));
    }

    #[test]
    fn detect_json_array() {
        assert_eq!(detect_content_type("[1, 2, 3]"), ContentType::JsonArray);
    }

    #[test]
    fn detect_json_object() {
        assert_eq!(detect_content_type("{\"a\": 1}"), ContentType::JsonObject);
    }

    #[test]
    fn detect_git_diff() {
        let diff = "@@ -1,3 +1,3 @@\n-foo\n+bar\n";
        assert_eq!(detect_content_type(diff), ContentType::GitDiff);
    }

    #[test]
    fn detect_html() {
        assert_eq!(detect_content_type("<html></html>"), ContentType::Html);
        assert_eq!(
            detect_content_type("<!DOCTYPE html>\n<html>"),
            ContentType::Html
        );
    }

    #[test]
    fn detect_search_results() {
        let results = "src/main.rs:42:fn main()\nsrc/lib.rs:10:pub fn foo()";
        assert_eq!(detect_content_type(results), ContentType::SearchResults);
    }

    #[test]
    fn detect_build_output() {
        let build = "error[E0000]: mismatched types\n  --> src/lib.rs:1:1\n";
        assert_eq!(detect_content_type(build), ContentType::BuildOutput);
    }

    #[test]
    fn detect_source_code() {
        let code = "fn main() {}\n\npub fn helper() {}\n";
        assert_eq!(detect_content_type(code), ContentType::SourceCode);
    }

    #[test]
    fn detect_plain_text_by_default() {
        assert_eq!(detect_content_type("hello world"), ContentType::PlainText);
        assert_eq!(detect_content_type(""), ContentType::PlainText);
    }

    #[test]
    fn content_type_from_str_roundtrips() {
        for ct in ContentType::ALL {
            let parsed: ContentType = ct.as_str().parse().unwrap();
            assert_eq!(ct, parsed);
        }
    }

    #[test]
    fn unknown_content_type_error_lists_supported_types() {
        let err: ContentTypeParseError = "xml".parse::<ContentType>().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("xml"));
        assert!(msg.contains("json_array"));
        assert!(msg.contains("plain_text"));
    }

    #[test]
    fn content_type_parse_error_exposes_unknown_value() {
        let err: ContentTypeParseError = "xml".parse::<ContentType>().unwrap_err();
        assert_eq!(err.value(), "xml");
    }

    #[test]
    fn content_type_parse_error_is_cloneable_and_equatable() {
        let err: ContentTypeParseError = "xml".parse::<ContentType>().unwrap_err();
        assert_eq!(err, err.clone());
    }

    #[test]
    fn content_type_from_str_is_case_insensitive_and_trims_whitespace() {
        assert_eq!(
            "  GIT_DIFF  ".parse::<ContentType>().unwrap(),
            ContentType::GitDiff
        );
        assert_eq!(
            "JSON_ARRAY".parse::<ContentType>().unwrap(),
            ContentType::JsonArray
        );
        assert_eq!(
            "Plain_Text".parse::<ContentType>().unwrap(),
            ContentType::PlainText
        );
    }

    #[test]
    fn content_type_display_matches_as_str() {
        for ct in ContentType::ALL {
            assert_eq!(format!("{ct}"), ct.as_str());
        }
    }

    #[test]
    fn content_type_label_is_human_readable() {
        assert_eq!(ContentType::JsonArray.label(), "json array");
        assert_eq!(ContentType::JsonObject.label(), "json object");
        assert_eq!(ContentType::SourceCode.label(), "source code");
        assert_eq!(ContentType::SearchResults.label(), "search results");
        assert_eq!(ContentType::BuildOutput.label(), "build output");
        assert_eq!(ContentType::GitDiff.label(), "git diff");
        assert_eq!(ContentType::Html.label(), "html");
        assert_eq!(ContentType::PlainText.label(), "plain text");
    }
}
