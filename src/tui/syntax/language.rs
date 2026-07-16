//! Language definitions for the syntax highlighter.
//!
//! Extracted from `mod.rs`: the `Language` enum and its dispatch —
//! detection from a code-fence label, comment/string delimiters,
//! and per-language keyword sets. Pure data, no highlighter state.

use std::collections::HashSet;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Language {
    #[default]
    Unknown,
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    C,
    Cpp,
    Java,
    Shell,
    Json,
    Yaml,
    Toml,
    Markdown,
}

impl Language {
    pub(super) fn from_str(lang: &str) -> Self {
        match lang.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Language::Rust,
            "python" | "py" => Language::Python,
            "javascript" | "js" | "jsx" => Language::JavaScript,
            "typescript" | "ts" | "tsx" => Language::TypeScript,
            "go" | "golang" => Language::Go,
            "c" => Language::C,
            "cpp" | "c++" | "cxx" => Language::Cpp,
            "java" => Language::Java,
            "shell" | "sh" | "bash" | "zsh" | "fish" => Language::Shell,
            "json" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "toml" => Language::Toml,
            "markdown" | "md" => Language::Markdown,
            _ => Language::Unknown,
        }
    }

    pub(super) fn line_comment(&self) -> Option<&'static str> {
        match self {
            Language::Shell | Language::Python | Language::Yaml | Language::Toml => Some("#"),
            Language::Rust
            | Language::JavaScript
            | Language::TypeScript
            | Language::Go
            | Language::C
            | Language::Cpp
            | Language::Java
            | Language::Json => Some("//"),
            Language::Markdown | Language::Unknown => None,
        }
    }

    pub(super) fn block_comment(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Language::Rust
            | Language::JavaScript
            | Language::TypeScript
            | Language::Go
            | Language::C
            | Language::Cpp
            | Language::Java
            | Language::Markdown => Some(("/*", "*/")),
            Language::Python => Some(("\"\"\"", "\"\"\"")),
            Language::Shell
            | Language::Json
            | Language::Yaml
            | Language::Toml
            | Language::Unknown => None,
        }
    }

    pub(super) fn string_quotes(&self) -> &[char] {
        match self {
            Language::Shell => &['"', '\''],
            Language::Python => &['"', '\''],
            _ => &['"', '\'', '`'],
        }
    }

    fn keywords(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "union", "unsafe", "use", "where", "while",
            ],
            Language::Python => &[
                "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
                "continue", "def", "del", "elif", "else", "except", "finally", "for", "from",
                "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass",
                "raise", "return", "try", "while", "with", "yield",
            ],
            Language::JavaScript | Language::TypeScript => &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "debugger",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "function",
                "if",
                "import",
                "in",
                "instanceof",
                "let",
                "new",
                "null",
                "of",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "typeof",
                "undefined",
                "var",
                "void",
                "while",
                "with",
                "yield",
            ],
            Language::Go => &[
                "break",
                "case",
                "chan",
                "const",
                "continue",
                "default",
                "defer",
                "else",
                "fallthrough",
                "for",
                "func",
                "go",
                "goto",
                "if",
                "import",
                "interface",
                "map",
                "package",
                "range",
                "return",
                "select",
                "struct",
                "switch",
                "true",
                "false",
                "nil",
                "type",
                "var",
            ],
            Language::C | Language::Cpp => &[
                "alignas",
                "alignof",
                "auto",
                "bool",
                "break",
                "case",
                "catch",
                "char",
                "class",
                "const",
                "constexpr",
                "continue",
                "default",
                "delete",
                "do",
                "double",
                "else",
                "enum",
                "explicit",
                "export",
                "extern",
                "false",
                "float",
                "for",
                "friend",
                "goto",
                "if",
                "inline",
                "int",
                "long",
                "mutable",
                "namespace",
                "new",
                "noexcept",
                "nullptr",
                "operator",
                "private",
                "protected",
                "public",
                "register",
                "reinterpret_cast",
                "return",
                "short",
                "signed",
                "sizeof",
                "static",
                "static_assert",
                "static_cast",
                "struct",
                "switch",
                "template",
                "this",
                "thread_local",
                "throw",
                "true",
                "try",
                "typedef",
                "typeid",
                "typename",
                "union",
                "unsigned",
                "using",
                "virtual",
                "void",
                "volatile",
                "wchar_t",
                "while",
            ],
            Language::Java => &[
                "abstract",
                "assert",
                "boolean",
                "break",
                "byte",
                "case",
                "catch",
                "char",
                "class",
                "const",
                "continue",
                "default",
                "do",
                "double",
                "else",
                "enum",
                "extends",
                "false",
                "final",
                "finally",
                "float",
                "for",
                "goto",
                "if",
                "implements",
                "import",
                "instanceof",
                "int",
                "interface",
                "long",
                "native",
                "new",
                "null",
                "package",
                "private",
                "protected",
                "public",
                "return",
                "short",
                "static",
                "strictfp",
                "super",
                "switch",
                "synchronized",
                "this",
                "throw",
                "throws",
                "transient",
                "true",
                "try",
                "void",
                "volatile",
                "while",
            ],
            Language::Shell => &[
                "alias", "break", "case", "continue", "do", "done", "echo", "elif", "else", "esac",
                "exit", "export", "false", "fi", "for", "function", "if", "in", "local", "printf",
                "read", "readonly", "return", "select", "shift", "source", "then", "true",
                "typeset", "unset", "until", "while",
            ],
            Language::Json
            | Language::Yaml
            | Language::Toml
            | Language::Markdown
            | Language::Unknown => &[],
        }
    }

    fn cache_index(&self) -> usize {
        match self {
            Language::Unknown => 0,
            Language::Rust => 1,
            Language::Python => 2,
            Language::JavaScript => 3,
            Language::TypeScript => 4,
            Language::Go => 5,
            Language::C => 6,
            Language::Cpp => 7,
            Language::Java => 8,
            Language::Shell => 9,
            Language::Json => 10,
            Language::Yaml => 11,
            Language::Toml => 12,
            Language::Markdown => 13,
        }
    }

    pub(super) fn keyword_set(&self) -> &'static HashSet<&'static str> {
        static KEYWORD_CACHE: [OnceLock<HashSet<&'static str>>; 14] = [
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
            OnceLock::new(),
        ];
        let idx = self.cache_index();
        KEYWORD_CACHE[idx].get_or_init(|| self.keywords().iter().copied().collect())
    }
}
