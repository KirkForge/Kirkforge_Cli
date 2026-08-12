//! R4 — port of `orchestrator/src/task-profile.ts`.
//!
//! Static task-language profiles + a regex `detect_task_profile` classifier.
//! Pure lookup tables; no fs, no model calls.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::correction::VerifierPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskLanguage {
    Typescript,
    Javascript,
    Python,
    Shell,
    Cpp,
    C,
    Rust,
    Go,
    Sql,
    Text,
}

impl TaskLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskLanguage::Typescript => "typescript",
            TaskLanguage::Javascript => "javascript",
            TaskLanguage::Python => "python",
            TaskLanguage::Shell => "shell",
            TaskLanguage::Cpp => "cpp",
            TaskLanguage::C => "c",
            TaskLanguage::Rust => "rust",
            TaskLanguage::Go => "go",
            TaskLanguage::Sql => "sql",
            TaskLanguage::Text => "text",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuredCheckCommand {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub append_files: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WritePolicy {
    #[serde(default)]
    pub allow_overwrite: bool,
    #[serde(default)]
    pub deny_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmissionSchema {
    pub language: TaskLanguage,
    pub default_file: String,
    pub fence_languages: Vec<String>,
    pub check_command: String,
    pub structured_check: Option<StructuredCheckCommand>,
    pub prompt_hint: String,
    pub allowed_extensions: Vec<String>,
    pub forbidden_extensions: Vec<String>,
    pub verifier_policy: VerifierPolicy,
    pub validator_required: bool,
    pub write_policy: Option<WritePolicy>,
}

pub type TaskProfile = EmissionSchema;

fn policy(required: &[&str], advisory: &[&str]) -> VerifierPolicy {
    VerifierPolicy {
        required: required.iter().map(|s| (*s).to_string()).collect(),
        advisory: advisory.iter().map(|s| (*s).to_string()).collect(),
        missing_required: vec![],
        skipped_required: vec![],
    }
}

fn build_profiles() -> std::collections::HashMap<TaskLanguage, TaskProfile> {
    use TaskLanguage::*;
    let mut m = std::collections::HashMap::new();
    m.insert(
        Typescript,
        EmissionSchema {
            language: Typescript,
            default_file: "output.ts".into(),
            fence_languages: vec!["typescript".into(), "ts".into()],
            check_command: "npx tsc --noEmit".into(),
            structured_check: Some(StructuredCheckCommand {
                command: "npx".into(),
                args: vec!["tsc".into(), "--noEmit".into()],
                append_files: false,
            }),
            prompt_hint: "Emit TypeScript files. Prefer .ts paths.".into(),
            allowed_extensions: [".ts", ".tsx", ".json", ".css", ".html", ".txt"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            forbidden_extensions: [".py", ".rs", ".go", ".sh"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&["lint", "types", "security"], &["graph"]),
            validator_required: false,
            write_policy: None,
        },
    );
    m.insert(
        Javascript,
        EmissionSchema {
            language: Javascript,
            default_file: "output.js".into(),
            fence_languages: vec!["javascript".into(), "js".into()],
            check_command: "node --check".into(),
            structured_check: Some(StructuredCheckCommand {
                command: "node".into(),
                args: vec!["--check".into()],
                append_files: true,
            }),
            prompt_hint: "Emit JavaScript files. Prefer .js paths.".into(),
            allowed_extensions: [".js", ".jsx", ".json", ".css", ".html", ".txt"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            forbidden_extensions: [".py", ".rs", ".go"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&["lint", "security"], &["types", "graph"]),
            validator_required: false,
            write_policy: None,
        },
    );
    m.insert(
        Python,
        EmissionSchema {
            language: Python,
            default_file: "solution.py".into(),
            fence_languages: vec!["python".into(), "py".into()],
            check_command: "python3 -m py_compile".into(),
            structured_check: Some(StructuredCheckCommand {
                command: "python3".into(),
                args: vec!["-m".into(), "py_compile".into()],
                append_files: true,
            }),
            prompt_hint: "Emit Python files. Prefer .py paths.".into(),
            allowed_extensions: [".py", ".txt", ".toml", ".cfg"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            forbidden_extensions: [".ts", ".tsx", ".js", ".jsx", ".d.ts"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&["lint", "types"], &["security", "graph"]),
            validator_required: false,
            write_policy: None,
        },
    );
    m.insert(
        Shell,
        EmissionSchema {
            language: Shell,
            default_file: "solution.sh".into(),
            fence_languages: vec!["bash".into(), "sh".into(), "shell".into()],
            check_command: "bash -n".into(),
            structured_check: Some(StructuredCheckCommand {
                command: "bash".into(),
                args: vec!["-n".into()],
                append_files: true,
            }),
            prompt_hint:
                "Emit POSIX shell files. Prefer .sh paths. Install shellcheck for best results."
                    .into(),
            allowed_extensions: [".sh", ".bash", ".txt"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            forbidden_extensions: [".ts", ".tsx", ".js", ".py", ".rs"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&["security", "lint"], &["types", "graph"]),
            validator_required: true,
            write_policy: None,
        },
    );
    m.insert(
        Cpp,
        EmissionSchema {
            language: Cpp,
            default_file: "solution.cpp".into(),
            fence_languages: vec!["cpp".into(), "c++".into()],
            check_command: "g++ -fsyntax-only".into(),
            structured_check: Some(StructuredCheckCommand {
                command: "g++".into(),
                args: vec!["-fsyntax-only".into()],
                append_files: true,
            }),
            prompt_hint: "Emit C++ files. Prefer .cpp paths.".into(),
            allowed_extensions: [".cpp", ".cc", ".cxx", ".h", ".hpp", ".txt"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            forbidden_extensions: [".ts", ".tsx", ".py"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&[], &["lint", "types", "security", "graph"]),
            validator_required: true,
            write_policy: None,
        },
    );
    m.insert(
        C,
        EmissionSchema {
            language: C,
            default_file: "solution.c".into(),
            fence_languages: vec!["c".into()],
            check_command: "gcc -fsyntax-only".into(),
            structured_check: Some(StructuredCheckCommand {
                command: "gcc".into(),
                args: vec!["-fsyntax-only".into()],
                append_files: true,
            }),
            prompt_hint: "Emit C files. Prefer .c paths.".into(),
            allowed_extensions: [".c", ".h", ".txt"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            forbidden_extensions: [".ts", ".tsx", ".py"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&[], &["lint", "types", "security", "graph"]),
            validator_required: true,
            write_policy: None,
        },
    );
    m.insert(
        Rust,
        EmissionSchema {
            language: Rust,
            default_file: "solution.rs".into(),
            fence_languages: vec!["rust".into(), "rs".into()],
            check_command: "rustc --emit=metadata".into(),
            structured_check: Some(StructuredCheckCommand {
                command: "rustc".into(),
                args: vec!["--emit=metadata".into()],
                append_files: true,
            }),
            prompt_hint: "Emit Rust files. Prefer .rs paths.".into(),
            allowed_extensions: [".rs", ".toml", ".txt"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            forbidden_extensions: [".ts", ".tsx", ".py"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&[], &["lint", "types", "security", "graph"]),
            validator_required: true,
            write_policy: None,
        },
    );
    m.insert(
        Go,
        EmissionSchema {
            language: Go,
            default_file: "main.go".into(),
            fence_languages: vec!["go".into()],
            check_command: "go vet ./...".into(),
            structured_check: Some(StructuredCheckCommand {
                command: "go".into(),
                args: vec!["vet".into(), "./...".into()],
                append_files: false,
            }),
            prompt_hint: "Emit Go files. Prefer .go paths.".into(),
            allowed_extensions: [".go", ".mod", ".sum", ".txt"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            forbidden_extensions: [".ts", ".tsx", ".py"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&[], &["lint", "types", "security", "graph"]),
            validator_required: true,
            write_policy: None,
        },
    );
    m.insert(
        Sql,
        EmissionSchema {
            language: Sql,
            default_file: "query.sql".into(),
            fence_languages: vec!["sql".into()],
            check_command: "".into(),
            structured_check: None,
            prompt_hint: "Emit SQL files. Prefer .sql paths.".into(),
            allowed_extensions: [".sql", ".txt"].iter().map(|s| (*s).to_string()).collect(),
            forbidden_extensions: [".ts", ".tsx", ".py"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            verifier_policy: policy(&[], &["lint", "types", "security", "graph"]),
            validator_required: true,
            write_policy: None,
        },
    );
    m.insert(
        Text,
        EmissionSchema {
            language: Text,
            default_file: "answer.txt".into(),
            fence_languages: vec!["text".into()],
            check_command: "".into(),
            structured_check: None,
            prompt_hint:
                "Emit .txt or .md. Other extensions require explicit --language or --validator."
                    .into(),
            allowed_extensions: [".txt", ".md"].iter().map(|s| (*s).to_string()).collect(),
            forbidden_extensions: vec![
                ".ts", ".tsx", ".js", ".jsx", ".py", ".rs", ".go", ".sh", ".bash", ".exe", ".dll",
                ".so", ".json", ".csv", ".yaml", ".yml", ".toml", ".xml", ".html", ".css",
            ]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
            verifier_policy: policy(&[], &["lint", "types", "security", "graph"]),
            validator_required: true,
            write_policy: Some(WritePolicy {
                allow_overwrite: false,
                deny_paths: vec![],
            }),
        },
    );
    m
}

static PROFILES: LazyLock<std::collections::HashMap<TaskLanguage, TaskProfile>> =
    LazyLock::new(build_profiles);

/// Lookup a profile by language. Panics only if the language is not in the
/// static table (i.e. a programmer error — every `TaskLanguage` variant has
/// an entry). Use `profile_for_language_opt` for fallible lookup.
pub fn profile_for_language(language: TaskLanguage) -> TaskProfile {
    PROFILES
        .get(&language)
        .expect("every TaskLanguage variant has a profile")
        .clone()
}

struct DetectionRule {
    language: TaskLanguage,
    pattern: &'static str,
}

const RULES: &[DetectionRule] = &[
    DetectionRule {
        language: TaskLanguage::Python,
        pattern: r"\b(?:python|py_compile|pytest|pandas|flask|django|cython|pip|requirements\.txt|csv|parquet|jupyter|notebook|classifier|debug.*program|broken-python|vul-flask)\b",
    },
    DetectionRule {
        language: TaskLanguage::Shell,
        pattern: r"\b(?:bash|shell|sh script|script|bucket|aws|s3|cron|unix|linux|command line|cli command|create-bucket)\b",
    },
    DetectionRule {
        language: TaskLanguage::Cpp,
        pattern: r"\b(?:c\+\+|cpp|g\+\+|clang\+\+|cmake|cpp-compatibility)\b",
    },
    DetectionRule {
        language: TaskLanguage::C,
        pattern: r"\b(?:gcc|clang|makefile|\.c\b|c program)\b",
    },
    DetectionRule {
        language: TaskLanguage::Rust,
        pattern: r"\b(?:rust|cargo|rustc)\b",
    },
    DetectionRule {
        language: TaskLanguage::Go,
        pattern: r"\b(?:golang|go test|go\.mod)\b",
    },
    DetectionRule {
        language: TaskLanguage::Sql,
        pattern: r"\b(?:sql|sqlite|postgres|query|database|simple-sql-query)\b",
    },
    DetectionRule {
        language: TaskLanguage::Javascript,
        pattern: r"\b(?:javascript|node\.?js|node --check|\.js|jsx)\b",
    },
    DetectionRule {
        language: TaskLanguage::Typescript,
        pattern: r"\b(?:typescript|\bts\b|tsc|\.ts|tsx|web scraper|form-filling|server endpoint|endpoint)\b",
    },
];

static DETECTION_SET: LazyLock<(regex::RegexSet, Vec<TaskLanguage>)> = LazyLock::new(|| {
    let patterns: Vec<String> = RULES.iter().map(|r| format!("(?i){}", r.pattern)).collect();
    let set = regex::RegexSet::new(patterns).expect("static detection patterns compile");
    let langs = RULES.iter().map(|r| r.language).collect();
    (set, langs)
});

/// Regex-based task-language classifier. Falls back to `Text`.
pub fn detect_task_profile(description: &str) -> TaskProfile {
    let (set, langs) = &*DETECTION_SET;
    if let Some(idx) = set.matches(description).iter().next() {
        return profile_for_language(langs[idx]);
    }
    profile_for_language(TaskLanguage::Text)
}

/// Maps a language name to its canonical file extension. Unknown → `.txt`.
pub fn extension_for_language(language: Option<&str>) -> &'static str {
    match language.map(str::to_lowercase).as_deref() {
        Some("python") => ".py",
        Some("shell") | Some("bash") | Some("sh") => ".sh",
        Some("cpp") | Some("c++") => ".cpp",
        Some("c") => ".c",
        Some("rust") => ".rs",
        Some("go") => ".go",
        Some("sql") => ".sql",
        Some("javascript") | Some("js") => ".js",
        Some("typescript") | Some("ts") => ".ts",
        _ => ".txt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_python_before_prompting() {
        assert_eq!(
            detect_task_profile("fix broken-python pandas csv-to-parquet script").language,
            TaskLanguage::Python
        );
    }

    #[test]
    fn detect_shell_aws_cli() {
        assert_eq!(
            detect_task_profile("create-bucket using aws cli shell commands").language,
            TaskLanguage::Shell
        );
    }

    #[test]
    fn detect_rust() {
        assert_eq!(
            detect_task_profile("build a rust cargo workspace").language,
            TaskLanguage::Rust
        );
    }

    #[test]
    fn detect_defaults_to_text() {
        assert_eq!(
            detect_task_profile("hello world").language,
            TaskLanguage::Text
        );
    }

    #[test]
    fn profile_for_language_round_trips() {
        for lang in [
            TaskLanguage::Typescript,
            TaskLanguage::Javascript,
            TaskLanguage::Python,
            TaskLanguage::Shell,
            TaskLanguage::Cpp,
            TaskLanguage::C,
            TaskLanguage::Rust,
            TaskLanguage::Go,
            TaskLanguage::Sql,
            TaskLanguage::Text,
        ] {
            let p = profile_for_language(lang);
            assert_eq!(p.language, lang);
            assert!(!p.default_file.is_empty());
        }
    }

    #[test]
    fn text_profile_forbids_executable_extensions() {
        let p = profile_for_language(TaskLanguage::Text);
        assert!(p.forbidden_extensions.contains(&".exe".to_string()));
        assert!(p.forbidden_extensions.contains(&".dll".to_string()));
        assert!(p.forbidden_extensions.contains(&".so".to_string()));
    }

    #[test]
    fn extension_for_language_known_and_unknown() {
        assert_eq!(extension_for_language(Some("python")), ".py");
        assert_eq!(extension_for_language(Some("c++")), ".cpp");
        assert_eq!(extension_for_language(Some("typescript")), ".ts");
        assert_eq!(extension_for_language(Some("bash")), ".sh");
        assert_eq!(extension_for_language(None), ".txt");
        assert_eq!(extension_for_language(Some("klingon")), ".txt");
    }
}
