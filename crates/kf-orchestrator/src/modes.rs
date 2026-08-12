//! Mode executors (R1). Port of `orchestrator/src/modes.ts` (hard-prompt +
//! schema-contract) and `orchestrator/src/artifact-mode.ts` (artifact +
//! JSONL parsing).
//!
//! Each executor takes a `&dyn ModelClient` and returns a `DelegationResult`.
//! The model call is the only impure step; everything else (parsing, signal
//! construction, write scheduling) is pure or `std::fs`-bound.

use std::path::Path;

use anyhow::Result;
use kf_routing::path_safety::{
    disallowed_artifact, extract_extension, final_file_is_symlink, is_binary_like_content,
    is_inside_cwd, segments_have_escaping_symlink, sha256_of, write_artifacts, ArtifactRecord,
    TaskProfileLike, WritePolicyLike, WriteResult,
};
use kf_routing::profile::{extension_for_language, TaskProfile};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::model::{ModelClient, TaskBrief};
use crate::sink::{ArtifactEvent, EventSink};
use crate::types::{DelegationDecisionInfo, DelegationResult, Emission, Signal};

// ── JSONL artifact protocol ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedArtifact {
    pub file_path: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParseResult {
    pub artifacts: Vec<ParsedArtifact>,
    pub strict_termination: bool,
    pub warnings: Vec<String>,
}

/// Decode a base64-standard string into UTF-8. Returns None on invalid base64.
fn b64_decode_to_string(b64: &str) -> Option<String> {
    use base64::Engine;
    if !b64
        .as_bytes()
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/' || *c == b'=')
    {
        return None;
    }
    let raw = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    String::from_utf8(raw).ok()
}

/// Parse a JSONL artifact stream. Hard-fails (strict_termination=false) on
/// sha256 mismatch, missing fields, or any non-JSON line. Mirrors TS
/// `parseJsonlArtifacts`. The legacy marker protocol (`### FILE:`) is
/// gated behind `allow_marker_fallback` and disabled by default.
pub fn parse_jsonl_artifacts(output: &str, allow_marker_fallback: bool) -> ParseResult {
    let mut artifacts = Vec::new();
    let mut strict = true;
    let mut warnings = Vec::new();

    for (i, raw_line) in output.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('{') {
            warnings.push(format!(
                "JSONL line {}: non-JSONL content in strict artifact stream",
                i + 1
            ));
            strict = false;
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                warnings.push(format!(
                    "JSONL line {}: not valid JSON — protocol integrity violation",
                    i + 1
                ));
                strict = false;
                continue;
            }
        };
        let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind != "file_write" {
            if obj.get("type").is_some() {
                warnings.push(format!(
                    "JSONL line {}: unknown artifact type \"{}\" — only \"file_write\" is recognized",
                    i + 1,
                    kind
                ));
                strict = false;
            }
            continue;
        }
        let path = match obj.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                warnings.push(format!("JSONL line {}: missing path", i + 1));
                strict = false;
                continue;
            }
        };
        // Prefer canonical content_b64; legacy plaintext content is gated.
        let content = if let Some(b64) = obj.get("content_b64").and_then(|v| v.as_str()) {
            match b64_decode_to_string(b64) {
                Some(s) => s,
                None => {
                    warnings.push(format!(
                        "line {}: JSONL artifact \"{}\" has invalid base64 content_b64",
                        i + 1,
                        path
                    ));
                    strict = false;
                    continue;
                }
            }
        } else if let Some(c) = obj.get("content").and_then(|v| v.as_str()) {
            warnings.push(format!(
                "line {}: JSONL artifact \"{}\" uses deprecated \"content\" field — content_b64 required",
                i + 1,
                path
            ));
            strict = false;
            c.to_string()
        } else {
            warnings.push(format!(
                "line {}: JSONL artifact \"{}\" missing both content_b64 and content fields",
                i + 1,
                path
            ));
            strict = false;
            continue;
        };

        let expected_hash = match obj.get("sha256").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s,
            _ => {
                warnings.push(format!(
                    "line {}: JSONL artifact \"{}\" missing required sha256",
                    i + 1,
                    path
                ));
                strict = false;
                continue;
            }
        };
        let actual_hash = sha256_of(&content);
        if actual_hash != expected_hash {
            warnings.push(format!(
                "line {}: JSONL sha256 mismatch for \"{}\": expected {}, got {}",
                i + 1,
                path,
                expected_hash,
                actual_hash
            ));
            strict = false;
            continue;
        }
        artifacts.push(ParsedArtifact {
            file_path: path.trim().to_string(),
            content,
        });
    }

    if !artifacts.is_empty() {
        return ParseResult {
            artifacts,
            strict_termination: strict,
            warnings,
        };
    }

    if allow_marker_fallback {
        return parse_artifacts(output);
    }

    // Distinguish three terminal cases:
    // - any warnings generated → at least one JSONL line was seen and rejected
    //   → return non-strict (the protocol was attempted but failed).
    // - no warnings but at least one `{`-prefixed line existed → all lines
    //   were filtered as unknown-type or empty objects → strict=true.
    // - nothing JSONL-shaped at all → "No JSONL artifact protocol detected".
    if !warnings.is_empty() {
        return ParseResult {
            artifacts,
            strict_termination: false,
            warnings,
        };
    }
    let has_any_jsonl = output.lines().any(|l| l.trim().starts_with('{'));
    if !has_any_jsonl {
        return ParseResult {
            artifacts: Vec::new(),
            strict_termination: false,
            warnings: vec!["No JSONL artifact protocol detected in output".to_string()],
        };
    }

    ParseResult {
        artifacts: Vec::new(),
        strict_termination: true,
        warnings,
    }
}

/// Marker-protocol parser (`### FILE:` / `### END`). Port of TS
/// `parseArtifacts`. Gated behind `allow_marker_fallback` by default.
pub fn parse_artifacts(output: &str) -> ParseResult {
    static FILE_MARKER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static END_MARKER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let file_re = FILE_MARKER.get_or_init(|| Regex::new(r"^### FILE:\s*(.+)$").unwrap());
    let end_re = END_MARKER.get_or_init(|| Regex::new(r"^### END\s*$").unwrap());

    let mut artifacts = Vec::new();
    let mut warnings = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_content: Vec<String> = Vec::new();

    for line in output.lines() {
        if let Some(c) = file_re.captures(line) {
            if let Some(path) = &current_path {
                if !current_content.is_empty() {
                    let had_marker_in_content =
                        current_content.iter().any(|cl| file_re.is_match(cl));
                    if had_marker_in_content {
                        warnings.push(format!(
                            "artifact \"{path}\" content contained a line matching \"### FILE:\" — possible marker collision, file may be truncated"
                        ));
                    }
                    artifacts.push(ParsedArtifact {
                        file_path: path.clone(),
                        content: strip_outer_fence(&current_content.join("\n")),
                    });
                }
            }
            current_path = Some(c[1].trim().to_string());
            current_content.clear();
            continue;
        }
        if end_re.is_match(line) {
            if let Some(path) = current_path.take() {
                artifacts.push(ParsedArtifact {
                    file_path: path,
                    content: strip_outer_fence(&current_content.join("\n")),
                });
                current_content.clear();
            }
            continue;
        }
        if current_path.is_some() {
            current_content.push(line.to_string());
        }
    }
    let unterminated_path = current_path.clone();
    if let Some(path) = current_path {
        if !current_content.is_empty() {
            artifacts.push(ParsedArtifact {
                file_path: path.clone(),
                content: strip_outer_fence(&current_content.join("\n")),
            });
            warnings.push(format!(
                "artifact \"{path}\" is unterminated — missing ### END marker"
            ));
        }
    }
    ParseResult {
        artifacts,
        strict_termination: unterminated_path.is_none(),
        warnings,
    }
}

fn strip_outer_fence(content: &str) -> String {
    let trimmed = content.trim();
    static FENCE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re =
        FENCE.get_or_init(|| Regex::new(r"^```[A-Za-z0-9_+#.-]*\s*\n([\s\S]*?)\n?```$").unwrap());
    if let Some(c) = re.captures(trimmed) {
        format!("{}\n", c[1].trim_end())
    } else {
        format!("{}\n", trimmed.trim_end())
    }
}

// ── fenced-codeblock persistence (modes.ts:persistCodeBlocks) ────────────────

/// Result of persisting fenced code blocks. Mirrors the TS return shape.
#[derive(Debug, Clone, Default)]
pub struct PersistOutcome {
    pub written: Vec<String>,
    pub blocked: Vec<(String, String)>,
    pub hashes: Vec<String>,
    pub file_bytes: Vec<i64>,
    pub before_hashes: Vec<Option<String>>,
    pub existed: Vec<bool>,
}

/// Extract fenced code blocks from raw model output and persist them into
/// `cwd` using the same safety primitives as artifact-mode. Mirrors TS
/// `persistCodeBlocks`. The largest non-empty block wins when `target_file`
/// is set and multiple blocks are emitted.
pub fn persist_code_blocks(
    content: &str,
    cwd: &str,
    profile: Option<&TaskProfile>,
    target_file: Option<&str>,
    force_overwrite: bool,
) -> PersistOutcome {
    static FENCE_LANG: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static FENCE_PLAIN: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re_lang =
        FENCE_LANG.get_or_init(|| Regex::new(r"```([A-Za-z0-9_+#.-]*)\s*\n([\s\S]*?)```").unwrap());
    let re_plain = FENCE_PLAIN.get_or_init(|| Regex::new(r"```\s*\n([\s\S]*?)```").unwrap());

    let mut blocks: Vec<(Option<String>, String)> = Vec::new();
    for c in re_lang.captures_iter(content) {
        let lang = c.get(1).map(|m| m.as_str().to_string());
        let body = c.get(2).map(|m| m.as_str()).unwrap_or("").to_string();
        blocks.push((lang, body));
    }
    if blocks.is_empty() {
        for c in re_plain.captures_iter(content) {
            let body = c.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
            blocks.push((None, body));
        }
    }

    let language = profile.map(|p| p.language.as_str());
    let ext = extension_for_language(language);
    let default_base = profile
        .map(|p| p.default_file.clone())
        .unwrap_or_else(|| format!("output{ext}"));
    let base_name = target_file.map(|s| s.to_string()).unwrap_or(default_base);

    // When caller pinned a target file with multiple blocks, keep only the
    // largest non-empty one.
    let mut block_indices: Vec<usize> = (0..blocks.len()).collect();
    if target_file.is_some() && blocks.len() > 1 {
        let mut sized: Vec<(usize, usize)> = (0..blocks.len())
            .map(|i| (i, blocks[i].1.trim().len()))
            .filter(|(_, size)| *size > 0)
            .collect();
        sized.sort_by(|a, b| b.1.cmp(&a.1));
        block_indices = sized.first().map(|(i, _)| vec![*i]).unwrap_or_default();
    }

    let mut out = PersistOutcome::default();
    let profile_like = profile.map(task_profile_to_like);
    let mut push_blocked = |name: &str, reason: String| {
        out.blocked.push((name.to_string(), reason));
    };

    static SUFFIX_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let suffix_re = SUFFIX_RE.get_or_init(|| Regex::new(r"\.(\w+)$").unwrap());

    for (n, &i) in block_indices.iter().enumerate() {
        let (_, body) = &blocks[i];
        let code = body.trim();
        let name = if let Some(target) = target_file {
            target.to_string()
        } else if blocks.len() == 1 {
            base_name.clone()
        } else {
            // base-N.ext with 1-based suffix; ponytail: regex replace keeps it short.
            let suffix = format!("-{}-{}", n + 1, i + 1);
            if suffix_re.is_match(&base_name) {
                suffix_re
                    .replace(&base_name, format!("$0{suffix}"))
                    .to_string()
            } else {
                format!("{base_name}{suffix}")
            }
        };
        let fp = Path::new(cwd).join(&name);
        let fp_str = fp.to_string_lossy().to_string();

        if !is_inside_cwd(&fp_str, cwd) {
            push_blocked(&name, format!("path escapes sandbox: {name}"));
            continue;
        }
        let artifact = ArtifactRecord {
            file_path: name.clone(),
            content: format!("{code}\n"),
        };
        if let Some(reason) = disallowed_artifact(&artifact, profile_like.as_ref()) {
            push_blocked(&name, reason);
            continue;
        }
        if segments_have_escaping_symlink(&fp_str, cwd) {
            push_blocked(&name, format!("symlink escape detected: {name}"));
            continue;
        }
        if final_file_is_symlink(&fp_str) {
            push_blocked(
                &name,
                format!("final path is symlink — writes would follow link outside sandbox: {name}"),
            );
            continue;
        }
        if code.len() + 1 > kf_routing::path_safety::MAX_ARTIFACT_BYTES {
            push_blocked(
                &name,
                format!(
                    "artifact exceeds {} byte limit: {name}",
                    kf_routing::path_safety::MAX_ARTIFACT_BYTES
                ),
            );
            continue;
        }
        if is_binary_like_content(code) {
            push_blocked(&name, format!("binary-like content detected: {name}"));
            continue;
        }

        let existed = Path::new(&fp).exists();
        if existed && !force_overwrite {
            let allow = profile_like
                .as_ref()
                .and_then(|p| p.write_policy.as_ref())
                .map(|w| w.allow_overwrite)
                .unwrap_or(false);
            if !allow {
                push_blocked(
                    &name,
                    format!("overwrite denied (allowOverwrite not enabled in writePolicy): {name}"),
                );
                continue;
            }
        }
        if let Some(deny) = profile_like.as_ref().and_then(|p| p.write_policy.as_ref()) {
            if !deny.deny_paths.is_empty() {
                let rel = pathdiff_lite(&fp, cwd);
                let hit = deny
                    .deny_paths
                    .iter()
                    .any(|d| rel == *d || rel.starts_with(&format!("{d}/")));
                if hit {
                    push_blocked(&name, format!("path denied by writePolicy: {name}"));
                    continue;
                }
            }
        }

        let before_hash = if existed {
            std::fs::read(&fp).ok().map(|b| {
                let mut h = Sha256::new();
                h.update(&b);
                hex(&h.finalize())
            })
        } else {
            None
        };

        if let Some(parent) = Path::new(&fp).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                push_blocked(&name, format!("write error: {e}"));
                continue;
            }
        }
        match atomic_write(&fp, &format!("{code}\n")) {
            Ok(()) => {
                let written = std::fs::read(&fp).unwrap_or_default();
                let bytes = written.len() as i64;
                let hash = {
                    let mut h = Sha256::new();
                    h.update(&written);
                    hex(&h.finalize())
                };
                out.written.push(name);
                out.hashes.push(hash);
                out.file_bytes.push(bytes);
                out.before_hashes.push(before_hash);
                out.existed.push(existed);
            }
            Err(e) => push_blocked(&name, format!("write error: {e}")),
        }
    }
    out
}

fn pathdiff_lite(target: &Path, base: &str) -> String {
    // Use the same lexical-diff helper exposed by path_safety via write_artifacts.
    // ponytail: re-derive inline rather than exposing a private fn from
    // path_safety; the only callers are this deny-path check.
    let t = target.to_string_lossy();
    let t = t.strip_prefix(base).unwrap_or(&t);
    t.trim_start_matches('/').to_string()
}

fn atomic_write(target: &Path, content: &str) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut tmp_name = target
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    tmp_name.push_str(&format!(".tmp.{nanos:x}.{n:x}"));
    let tmp = target.with_file_name(tmp_name);
    std::fs::write(&tmp, content)?;
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
        drop(f);
    }
    std::fs::rename(&tmp, target)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn task_profile_to_like(p: &TaskProfile) -> TaskProfileLike {
    TaskProfileLike {
        language: Some(p.language.as_str().to_string()),
        allowed_extensions: Some(p.allowed_extensions.clone()),
        forbidden_extensions: Some(p.forbidden_extensions.clone()),
        write_policy: p.write_policy.as_ref().map(|w| WritePolicyLike {
            allow_overwrite: w.allow_overwrite,
            deny_paths: w.deny_paths.clone(),
        }),
    }
}

// ── Signal helpers ───────────────────────────────────────────────────────────

fn now_iso() -> String {
    // Ponytail: avoid chrono. SystemTime → seconds → RFC3339-ish UTC. The
    // orchestrator only uses this for signal timestamps (informational).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("t{secs}")
}

fn file_signal_value(
    written: &[String],
    hashes: &[String],
    file_bytes: &[i64],
    before_hashes: &[Option<String>],
    existed: &[bool],
    language: &str,
) -> serde_json::Value {
    let files = written
        .iter()
        .enumerate()
        .map(|(i, w)| {
            json!({
                "path": w,
                "sha256": hashes.get(i).cloned().unwrap_or_default(),
                "bytes": file_bytes.get(i).copied().unwrap_or(0),
                "beforeHash": before_hashes.get(i).cloned().flatten(),
                "existed": existed.get(i).copied().unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "filesWritten": written.len(),
        "totalBytes": file_bytes.iter().sum::<i64>(),
        "files": files,
        "language": language,
    })
}

// ── Mode executors ───────────────────────────────────────────────────────────

/// Hard-prompt mode: model emits text with fenced code blocks; we persist
/// them into `cwd`. Port of `modes.ts::executeHardPrompt`.
pub async fn execute_hard_prompt(
    client: &dyn ModelClient,
    brief: TaskBrief,
    task_id: &str,
    cwd: &str,
    profile: Option<&TaskProfile>,
    target_file: Option<&str>,
    sink: Option<&dyn EventSink>,
) -> Result<DelegationResult> {
    let emission = client.execute(&brief).await?;
    Ok(finalize_hard_prompt(
        emission,
        task_id,
        cwd,
        profile,
        target_file,
        sink,
    ))
}

/// Pure post-processing for hard-prompt mode — split out so tests can drive
/// it without a ModelClient.
pub fn finalize_hard_prompt(
    emission: Emission,
    task_id: &str,
    cwd: &str,
    profile: Option<&TaskProfile>,
    target_file: Option<&str>,
    sink: Option<&dyn EventSink>,
) -> DelegationResult {
    let language = profile
        .map(|p| p.language.as_str())
        .unwrap_or("unknown")
        .to_string();
    let was_truncated = emission.was_truncated();
    let force_overwrite = target_file.is_some();
    let persist = if was_truncated {
        PersistOutcome::default()
    } else {
        persist_code_blocks(
            &emission.content,
            cwd,
            profile,
            target_file,
            force_overwrite,
        )
    };
    let truncation_warning = was_truncated.then(|| {
        format!(
            "model output was truncated (finish_reason: {}) — file content may be incomplete",
            emission.finish_reason.as_deref().unwrap_or("?")
        )
    });

    let mut signals: Vec<Signal> = Vec::new();
    let agent_id = emission.agent_id.clone();
    let ts = now_iso();

    signals.push(Signal {
        id: format!("sig-{task_id}"),
        task_id: task_id.to_string(),
        domain: "task".into(),
        kind: "emission".into(),
        source: agent_id.clone(),
        ts: ts.clone(),
        value: json!({ "content": emission.content.chars().take(200).collect::<String>() }),
        confidence: None,
    });
    signals.push(Signal {
        id: format!("sig-files-{task_id}"),
        task_id: task_id.to_string(),
        domain: "code".into(),
        kind: "files.written".into(),
        source: agent_id.clone(),
        ts: ts.clone(),
        value: file_signal_value(
            &persist.written,
            &persist.hashes,
            &persist.file_bytes,
            &persist.before_hashes,
            &persist.existed,
            &language,
        ),
        confidence: None,
    });
    signals.push(Signal {
        id: format!("sig-emitted-{task_id}"),
        task_id: task_id.to_string(),
        domain: "code".into(),
        kind: "artifact.emitted".into(),
        source: agent_id.clone(),
        ts: ts.clone(),
        value: file_signal_value(
            &persist.written,
            &persist.hashes,
            &persist.file_bytes,
            &persist.before_hashes,
            &persist.existed,
            &language,
        ),
        confidence: None,
    });
    if !persist.blocked.is_empty() {
        let value = json!({
            "blockedPaths": persist.blocked.iter().map(|(p, r)| json!({"path": p, "reason": r})).collect::<Vec<_>>(),
        });
        signals.push(Signal {
            id: format!("sig-blocked-{task_id}"),
            task_id: task_id.to_string(),
            domain: "code".into(),
            kind: "artifact.blocked".into(),
            source: agent_id.clone(),
            ts: ts.clone(),
            value,
            confidence: None,
        });
    }
    if let Some(w) = truncation_warning {
        signals.push(Signal {
            id: format!("sig-truncated-{task_id}"),
            task_id: task_id.to_string(),
            domain: "code".into(),
            kind: "artifact.truncated".into(),
            source: agent_id.clone(),
            ts: ts.clone(),
            value: json!({
                "finishReason": emission.finish_reason.clone(),
                "warnings": [w],
            }),
            confidence: None,
        });
    }

    // Re-emit artifact events through the sink (if any). Synchronous from the
    // caller's POV; we drive the async sink via `tokio::runtime::Handle`'s
    // block_in_place workaround — but block_in_place panics in single-threaded
    // tests. Ponytail: emit async by collecting into a future the caller
    // awaits separately. To keep this fn pure+sync, we instead *schedule* the
    // emit by recording the events on the result; the orchestrator flushes
    // them from an async context (see `delegate::finalize_delegation`).
    let _ = sink; // sink emit happens in finalize_delegation

    let decision = DelegationDecisionInfo {
        mode: "hard-prompt".into(),
        reason: format!(
            "hard-prompt delegation: {} files written{}",
            persist.written.len(),
            if persist.blocked.is_empty() {
                String::new()
            } else {
                format!(", {} blocked", persist.blocked.len())
            }
        ),
        auto_routed: true,
    };
    DelegationResult {
        decision,
        emission,
        signals,
        packet: None,
        provider_resolved: None,
        skills_loaded: None,
    }
}

/// Schema-contract mode: model emits structured output. We do not persist
/// files; the schema_contract is carried on the emission. Port of
/// `modes.ts::executeSchemaContract`.
pub async fn execute_schema_contract(
    client: &dyn ModelClient,
    brief: TaskBrief,
    task_id: &str,
) -> Result<DelegationResult> {
    let emission = client.execute(&brief).await?;
    Ok(finalize_schema_contract(emission, task_id))
}

/// Pure post-processing for schema-contract mode.
pub fn finalize_schema_contract(emission: Emission, task_id: &str) -> DelegationResult {
    if emission.schema_contract.is_none() {
        warn!(
            "schema-contract delegation produced no schema_contract (task_id={})",
            task_id
        );
    }
    let was_truncated = emission.was_truncated();
    let truncation_warning = was_truncated.then(|| {
        format!(
            "model output was truncated (finish_reason: {}) — schema contract output may be incomplete",
            emission.finish_reason.as_deref().unwrap_or("?")
        )
    });
    let agent_id = emission.agent_id.clone();
    let ts = now_iso();
    let mut signals = vec![
        Signal {
            id: format!("sig-{task_id}"),
            task_id: task_id.to_string(),
            domain: "task".into(),
            kind: "emission".into(),
            source: agent_id.clone(),
            ts: ts.clone(),
            value: json!({ "content": emission.content.chars().take(200).collect::<String>() }),
            confidence: None,
        },
        Signal {
            id: format!("sig-ts-{task_id}"),
            task_id: task_id.to_string(),
            domain: "quality".into(),
            kind: "schema.validated".into(),
            source: agent_id.clone(),
            ts: ts.clone(),
            value: json!({ "validated": emission.schema_contract.is_some() }),
            confidence: Some(if was_truncated { 0.4 } else { 0.95 }),
        },
    ];
    if let Some(w) = truncation_warning {
        signals.push(Signal {
            id: format!("sig-truncated-{task_id}"),
            task_id: task_id.to_string(),
            domain: "code".into(),
            kind: "artifact.truncated".into(),
            source: agent_id.clone(),
            ts: ts.clone(),
            value: json!({
                "finishReason": emission.finish_reason.clone(),
                "warnings": [w],
            }),
            confidence: None,
        });
    }
    let decision = DelegationDecisionInfo {
        mode: "schema-contract".into(),
        reason: "structured verification".into(),
        auto_routed: true,
    };
    DelegationResult {
        decision,
        emission,
        signals,
        packet: None,
        provider_resolved: None,
        skills_loaded: None,
    }
}

/// Artifact mode: model emits JSONL artifacts; we parse + write them via
/// `kf_routing::path_safety::write_artifacts`. Port of
/// `artifact-mode.ts::executeArtifact`.
pub async fn execute_artifact(
    client: &dyn ModelClient,
    brief: TaskBrief,
    task_id: &str,
    cwd: &str,
    profile: Option<&TaskProfile>,
    allow_marker_fallback: bool,
) -> Result<DelegationResult> {
    let emission = client.execute(&brief).await?;
    Ok(finalize_artifact(
        emission,
        task_id,
        cwd,
        profile,
        allow_marker_fallback,
    ))
}

/// Pure post-processing for artifact mode.
pub fn finalize_artifact(
    emission: Emission,
    task_id: &str,
    cwd: &str,
    profile: Option<&TaskProfile>,
    allow_marker_fallback: bool,
) -> DelegationResult {
    let parsed = parse_jsonl_artifacts(&emission.content, allow_marker_fallback);
    let was_truncated = emission.was_truncated();
    let protocol_broken = !parsed.strict_termination || was_truncated;

    let protocol_reason = [
        if !parsed.strict_termination {
            Some("unterminated artifact block".to_string())
        } else {
            None
        },
        if was_truncated {
            Some(format!(
                "truncated model output (finish_reason: {})",
                emission.finish_reason.as_deref().unwrap_or("?")
            ))
        } else {
            None
        },
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" + ");

    let profile_like = profile.map(task_profile_to_like);
    let writes: Vec<WriteResult> = if protocol_broken {
        let reason = if parsed.warnings.is_empty() {
            protocol_reason.clone()
        } else {
            format!(
                "{} — parse warnings: {}",
                protocol_reason,
                parsed
                    .warnings
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        };
        parsed
            .artifacts
            .iter()
            .map(|a| WriteResult {
                file_path: a.file_path.clone(),
                bytes: 0,
                ok: false,
                blocked: Some(reason.clone()),
                warning: None,
                sha256: None,
                before_hash: None,
                existed: None,
            })
            .collect()
    } else {
        let records: Vec<ArtifactRecord> = parsed
            .artifacts
            .iter()
            .map(|a| ArtifactRecord {
                file_path: a.file_path.clone(),
                content: a.content.clone(),
            })
            .collect();
        write_artifacts(&records, cwd, profile_like.as_ref())
    };

    let mut all_warnings = parsed.warnings.clone();
    if was_truncated {
        all_warnings.push(format!(
            "model output was truncated (finish_reason: {}) — artifact content may be incomplete",
            emission.finish_reason.as_deref().unwrap_or("?")
        ));
    }
    if protocol_broken && !parsed.artifacts.is_empty() {
        all_warnings.push(format!(
            "protocol integrity violation: all {} artifact(s) blocked from write",
            parsed.artifacts.len()
        ));
    }

    let ok_writes: Vec<&WriteResult> = writes.iter().filter(|w| w.ok).collect();
    let blocked_writes: Vec<&WriteResult> = writes.iter().filter(|w| w.blocked.is_some()).collect();
    for w in &writes {
        if let Some(warn_msg) = &w.warning {
            all_warnings.push(warn_msg.clone());
        }
    }

    let language = profile
        .map(|p| p.language.as_str())
        .unwrap_or("unknown")
        .to_string();
    let agent_id = emission.agent_id.clone();
    let ts = now_iso();

    let files_value = ok_writes
        .iter()
        .map(|w| {
            json!({
                "path": w.file_path,
                "sha256": w.sha256.clone().unwrap_or_default(),
                "bytes": w.bytes,
                "beforeHash": w.before_hash.clone().flatten(),
                "existed": w.existed.unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();
    let total_bytes: i64 = writes.iter().map(|w| w.bytes as i64).sum();
    let confidence = if writes.iter().all(|w| w.ok) {
        0.9
    } else {
        0.4
    };

    let mut signals: Vec<Signal> = Vec::new();
    signals.push(Signal {
        id: format!("sig-{task_id}"),
        task_id: task_id.to_string(),
        domain: "task".into(),
        kind: "emission".into(),
        source: agent_id.clone(),
        ts: ts.clone(),
        value: json!({ "content": emission.content.chars().take(200).collect::<String>() }),
        confidence: None,
    });
    signals.push(Signal {
        id: format!("sig-artifact-{task_id}"),
        task_id: task_id.to_string(),
        domain: "code".into(),
        kind: "artifact.emitted".into(),
        source: agent_id.clone(),
        ts: ts.clone(),
        value: json!({
            "filesWritten": ok_writes.len(),
            "totalBytes": total_bytes,
            "files": files_value,
            "language": language,
        }),
        confidence: Some(confidence),
    });
    if !parsed.strict_termination {
        signals.push(Signal {
            id: format!("sig-unterminated-{task_id}"),
            task_id: task_id.to_string(),
            domain: "code".into(),
            kind: "artifact.unterminated".into(),
            source: agent_id.clone(),
            ts: ts.clone(),
            value: json!({ "warnings": all_warnings.clone() }),
            confidence: None,
        });
    }
    if was_truncated {
        signals.push(Signal {
            id: format!("sig-truncated-{task_id}"),
            task_id: task_id.to_string(),
            domain: "code".into(),
            kind: "artifact.truncated".into(),
            source: agent_id.clone(),
            ts: ts.clone(),
            value: json!({
                "finishReason": emission.finish_reason.clone(),
                "warnings": all_warnings.clone(),
            }),
            confidence: None,
        });
    }
    if !blocked_writes.is_empty() {
        signals.push(Signal {
            id: format!("sig-blocked-{task_id}"),
            task_id: task_id.to_string(),
            domain: "code".into(),
            kind: "artifact.blocked".into(),
            source: agent_id.clone(),
            ts: ts.clone(),
            value: json!({
                "blockedPaths": blocked_writes.iter().map(|b| json!({
                    "path": b.file_path,
                    "reason": b.blocked.clone().unwrap_or_default(),
                })).collect::<Vec<_>>(),
                "parseWarnings": all_warnings.clone(),
            }),
            confidence: None,
        });
    }

    let mut reason = format!("artifact emission: {} files written", ok_writes.len());
    if !blocked_writes.is_empty() {
        reason.push_str(&format!(", {} blocked", blocked_writes.len()));
    }
    if !parsed.strict_termination {
        reason.push_str(" (unterminated)");
    }
    if was_truncated {
        reason.push_str(" (truncated)");
    }
    let decision = DelegationDecisionInfo {
        mode: "artifact".into(),
        reason,
        auto_routed: true,
    };
    DelegationResult {
        decision,
        emission,
        signals,
        packet: None,
        provider_resolved: None,
        skills_loaded: None,
    }
}

/// Drive the EventSink for a finalized DelegationResult. Walks the signals
/// and emits artifact.* events for each. Port of `orchestrator-finalize.ts`
/// event-fanout loop.
pub async fn flush_signals_to_sink(result: &DelegationResult, sink: &dyn EventSink) {
    for sig in &result.signals {
        let event = match sig.kind.as_str() {
            "artifact.blocked" => ArtifactEvent::Blocked {
                task_id: sig.task_id.clone(),
                stream_id: sig.id.clone(),
                timestamp: sig.ts.clone(),
                value: sig.value.clone(),
            },
            "artifact.unterminated" => ArtifactEvent::Unterminated {
                task_id: sig.task_id.clone(),
                stream_id: sig.id.clone(),
                timestamp: sig.ts.clone(),
                value: sig.value.clone(),
            },
            "artifact.truncated" => ArtifactEvent::Truncated {
                task_id: sig.task_id.clone(),
                stream_id: sig.id.clone(),
                timestamp: sig.ts.clone(),
                value: sig.value.clone(),
            },
            "artifact.emitted" => ArtifactEvent::Emitted {
                task_id: sig.task_id.clone(),
                stream_id: sig.id.clone(),
                timestamp: sig.ts.clone(),
                value: sig.value.clone(),
            },
            _ => continue,
        };
        sink.emit(event).await;
    }
}

// keep the unused-extension helper import live for symmetry with TS path-safety
#[allow(dead_code)]
fn _keep_extract_extension() {
    let _ = extract_extension("x.py");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RecordingClient;
    use kf_routing::profile::profile_for_language;
    use tempfile::tempdir;

    fn emission(content: &str) -> Emission {
        Emission {
            agent_id: "agent-1".into(),
            content: content.into(),
            model: "test-model".into(),
            format: "hard-prompt".into(),
            total_tokens: 10,
            ..Default::default()
        }
    }

    // ── parse_jsonl_artifacts ──

    #[test]
    fn jsonl_parses_one_valid_artifact() {
        let content = "print('hi')\n";
        let hash = sha256_of(content);
        let payload = format!(
            r#"{{"type":"file_write","path":"solution.py","content_b64":"{}","sha256":"{}"}}"#,
            base64_b64(content),
            hash
        );
        let r = parse_jsonl_artifacts(&payload, false);
        assert!(r.strict_termination);
        assert_eq!(r.artifacts.len(), 1);
        assert_eq!(r.artifacts[0].file_path, "solution.py");
        assert_eq!(r.artifacts[0].content, content);
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn jsonl_hash_mismatch_marks_non_strict() {
        let payload = format!(
            r#"{{"type":"file_write","path":"x.py","content_b64":"{}","sha256":"deadbeef"}}"#,
            base64_b64("x")
        );
        let r = parse_jsonl_artifacts(&payload, false);
        assert!(!r.strict_termination);
        assert!(r.artifacts.is_empty());
        assert!(r.warnings[0].contains("sha256 mismatch"));
    }

    #[test]
    fn jsonl_missing_sha256_marks_non_strict() {
        let payload = format!(
            r#"{{"type":"file_write","path":"x.py","content_b64":"{}"}}"#,
            base64_b64("x")
        );
        let r = parse_jsonl_artifacts(&payload, false);
        assert!(!r.strict_termination);
        assert!(r.warnings[0].contains("missing required sha256"));
    }

    #[test]
    fn jsonl_invalid_base64_marks_non_strict() {
        let payload =
            r#"{"type":"file_write","path":"x.py","content_b64":"!!!not-b64!!!","sha256":"x"}"#;
        let r = parse_jsonl_artifacts(payload, false);
        assert!(!r.strict_termination);
        assert!(r.warnings[0].contains("invalid base64"));
    }

    #[test]
    fn jsonl_unknown_type_marks_non_strict() {
        let payload = r#"{"type":"weird_thing","path":"x"}"#;
        let r = parse_jsonl_artifacts(payload, false);
        assert!(!r.strict_termination);
        assert!(r.warnings[0].contains("unknown artifact type"));
    }

    #[test]
    fn jsonl_non_json_line_marks_non_strict() {
        let r = parse_jsonl_artifacts("hello world\n", false);
        assert!(!r.strict_termination);
        assert!(r.warnings[0].contains("non-JSONL content"));
    }

    #[test]
    fn jsonl_empty_output_no_protocol() {
        let r = parse_jsonl_artifacts("", false);
        assert!(!r.strict_termination);
        assert!(r.warnings[0].contains("No JSONL artifact protocol"));
    }

    #[test]
    fn jsonl_marker_fallback_disabled_by_default() {
        let r = parse_jsonl_artifacts("### FILE: a.py\nprint('x')\n### END\n", false);
        // No JSONL lines → falls into "no protocol" branch.
        assert!(r.artifacts.is_empty());
        assert!(!r.strict_termination);
    }

    #[test]
    fn marker_fallback_when_enabled() {
        let r = parse_artifacts("### FILE: a.py\nprint('x')\n### END\n");
        assert_eq!(r.artifacts.len(), 1);
        assert_eq!(r.artifacts[0].file_path, "a.py");
        assert_eq!(r.artifacts[0].content, "print('x')\n");
        assert!(r.strict_termination);
    }

    #[test]
    fn marker_unterminated_emits_warning() {
        let r = parse_artifacts("### FILE: a.py\nprint('x')");
        assert_eq!(r.artifacts.len(), 1);
        assert!(!r.strict_termination);
        assert!(r.warnings[0].contains("unterminated"));
    }

    fn base64_b64(s: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    // ── persist_code_blocks ──

    #[test]
    fn persist_writes_single_fenced_block() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let p = profile_for_language(kf_routing::TaskLanguage::Python);
        let out = persist_code_blocks(
            "```python\nprint('hello')\n```\n",
            &cwd,
            Some(&p),
            None,
            false,
        );
        assert_eq!(out.written, vec!["solution.py".to_string()]);
        assert!(out.blocked.is_empty());
        let written = std::fs::read_to_string(dir.path().join("solution.py")).unwrap();
        assert_eq!(written, "print('hello')\n");
    }

    #[test]
    fn persist_target_file_overrides_default() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let out = persist_code_blocks(
            "```python\nprint('hi')\n```\n",
            &cwd,
            None,
            Some("custom.py"),
            false,
        );
        assert_eq!(out.written, vec!["custom.py".to_string()]);
        assert!(dir.path().join("custom.py").exists());
    }

    #[test]
    fn persist_target_file_picks_largest_block_when_multiple() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let content = "```\nsmall\n```\n\n```\nthis is the larger block with more chars\n```\n";
        let out = persist_code_blocks(content, &cwd, None, Some("out.txt"), false);
        assert_eq!(out.written.len(), 1);
        let written = std::fs::read_to_string(dir.path().join("out.txt")).unwrap();
        assert!(written.contains("larger block"));
    }

    #[test]
    fn persist_blocks_path_escape() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let out = persist_code_blocks(
            "```python\nprint('x')\n```\n",
            &cwd,
            None,
            Some("../escape.py"),
            false,
        );
        assert!(out.written.is_empty());
        assert!(out
            .blocked
            .iter()
            .any(|(_, r)| r.contains("escapes sandbox")));
    }

    // ── execute_hard_prompt ──

    #[tokio::test]
    async fn hard_prompt_persists_emitted_code() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let content = "```python\nprint('hi')\n```\n";
        let client = RecordingClient::constant(emission(content));
        let p = profile_for_language(kf_routing::TaskLanguage::Python);
        let result = execute_hard_prompt(
            &client,
            TaskBrief::default(),
            "t1",
            &cwd,
            Some(&p),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.decision.mode, "hard-prompt");
        assert!(dir.path().join("solution.py").exists());
        let kinds: Vec<&str> = result.signals.iter().map(|s| s.kind.as_str()).collect();
        assert!(kinds.contains(&"files.written"));
        assert!(kinds.contains(&"artifact.emitted"));
    }

    // ── execute_artifact ──

    #[tokio::test]
    async fn artifact_writes_valid_jsonl() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        let content = "print('hi')\n";
        let hash = sha256_of(content);
        let body = format!(
            r#"{{"type":"file_write","path":"solution.py","content_b64":"{}","sha256":"{}"}}"#,
            base64_b64(content),
            hash
        );
        let client = RecordingClient::constant(emission(&body));
        let p = profile_for_language(kf_routing::TaskLanguage::Python);
        let result = execute_artifact(&client, TaskBrief::default(), "t1", &cwd, Some(&p), false)
            .await
            .unwrap();
        assert!(dir.path().join("solution.py").exists());
        assert_eq!(result.decision.mode, "artifact");
    }

    #[tokio::test]
    async fn artifact_blocks_all_when_protocol_broken() {
        let dir = tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().to_string();
        // Truncated emission with valid JSONL → protocol_broken=true → all writes blocked.
        let content = "print('hi')\n";
        let hash = sha256_of(content);
        let body = format!(
            r#"{{"type":"file_write","path":"solution.py","content_b64":"{}","sha256":"{}"}}"#,
            base64_b64(content),
            hash
        );
        let mut e = emission(&body);
        e.finish_reason = Some("length".into());
        let client = RecordingClient::constant(e);
        let p = profile_for_language(kf_routing::TaskLanguage::Python);
        let result = execute_artifact(&client, TaskBrief::default(), "t1", &cwd, Some(&p), false)
            .await
            .unwrap();
        assert!(!dir.path().join("solution.py").exists());
        let blocked = result.signals.iter().find(|s| s.kind == "artifact.blocked");
        assert!(blocked.is_some(), "expected blocked signal");
    }

    // ── execute_schema_contract ──

    #[tokio::test]
    async fn schema_contract_carries_schema_on_emission() {
        let mut e = emission("{}");
        e.format = "schema-contract".into();
        e.schema_contract = Some(json!({"fields": []}));
        let client = RecordingClient::constant(e);
        let result = execute_schema_contract(&client, TaskBrief::default(), "t1")
            .await
            .unwrap();
        assert_eq!(result.decision.mode, "schema-contract");
        let validated = result
            .signals
            .iter()
            .find(|s| s.kind == "schema.validated")
            .unwrap();
        assert_eq!(validated.value, json!({"validated": true}));
    }

    // ── flush_signals_to_sink ──

    #[tokio::test]
    async fn flush_emits_all_artifact_kinds_present() {
        use crate::sink::RecordingSink;
        let sink = RecordingSink::new();
        let mut result = DelegationResult::default();
        for kind in [
            "artifact.emitted",
            "artifact.blocked",
            "artifact.unterminated",
            "artifact.truncated",
        ] {
            result.signals.push(Signal {
                id: format!("s-{kind}"),
                task_id: "t".into(),
                domain: "code".into(),
                kind: kind.into(),
                source: "agent".into(),
                ts: "now".into(),
                value: json!({}),
                confidence: None,
            });
        }
        flush_signals_to_sink(&result, &sink).await;
        let mut kinds = sink.kinds();
        kinds.sort();
        let mut want = vec![
            "artifact.blocked",
            "artifact.emitted",
            "artifact.truncated",
            "artifact.unterminated",
        ];
        want.sort();
        assert_eq!(kinds, want);
    }
}
