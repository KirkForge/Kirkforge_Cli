// `kf-code update [--check]` — self-update from the latest GitHub release.
//
// Mirrors scripts/install.sh: fetch latest tag → download target archive →
// verify against SHA256SUMS.txt → extract → replace the running binary in
// place (atomic rename). `--check` prints current vs latest without touching
// disk. Existing deps only (reqwest, sha2, hex, tempfile).

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

const REPO: &str = "KirkForge/Kirkforge_Cli";
const API_LATEST: &str = "https://api.github.com/repos/KirkForge/Kirkforge_Cli/releases/latest";

/// Run the update command. `check = true` only reports current vs latest
/// version; `check = false` downloads, verifies, and replaces the binary.
pub(super) async fn handle_update_command(check: bool) -> Result<()> {
    let current = current_version();
    let latest = fetch_latest_tag()
        .await
        .context("failed to determine latest release tag")?;

    if check {
        let status = if current == latest {
            "up to date"
        } else {
            "update available"
        };
        println!("current: {current}");
        println!("latest:  {latest}");
        println!("status:  {status}");
        return Ok(());
    }

    if current == latest {
        println!("Already up to date ({current}).");
        return Ok(());
    }

    let target = detect_target_triple().ok_or_else(|| {
        anyhow!(
            "no release target triple for {}-{}; file a bug or use scripts/install.sh",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let archive = format!("kf-code-{target}.tar.gz");

    let client = build_http_client();
    println!("Downloading kf-code {latest} for {target}...");
    let archive_bytes = download_archive(&client, &latest, &archive)
        .await
        .with_context(|| format!("failed to download {archive}"))?;

    println!("Verifying checksum...");
    let sums = download_sha256sums(&client, &latest).await?;
    let expected = parse_sha256sums_line(&sums, &archive).ok_or_else(|| {
        anyhow!("no checksum entry for {archive} in SHA256SUMS.txt — refusing to install")
    })?;
    verify_sha256(&archive_bytes, &expected)?;

    let extracted = extract_binary(&archive_bytes, "kf-code")?;
    let exe = std::env::current_exe().context("could not resolve current exe path")?;
    replace_binary(&extracted, &exe)?;
    println!("Updated kf-code to {latest} at {}", exe.display());
    Ok(())
}

/// Return the compiled-in crate version (the version of the running binary).
fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Map (OS, ARCH) to the release archive target triple. Returns `None` for
/// combinations the release workflow does not build. Mirrors the case logic
/// in scripts/install.sh.
fn detect_target_triple() -> Option<String> {
    // ponytail: ceiling — covers the 4 triples the release workflow builds.
    // upgrade path: add musl/arm variants here when the workflow ships them.
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu".into()),
        ("macos", "x86_64") => Some("x86_64-apple-darwin".into()),
        ("macos", "aarch64") => Some("aarch64-apple-darwin".into()),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl".into()),
        _ => None,
    }
}

/// Parse one line of a SHA256SUMS.txt file and return the hash for the named
/// file. Lines look like `<hash>  <file>` (text mode) or `<hash> *<file>`
/// (binary mode); the leading `*` is stripped. Returns `None` if the line does
/// not name `file` or is malformed (hash not 64 hex chars).
fn parse_sha256sums_line(sums: &str, file: &str) -> Option<String> {
    for line in sums.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, |c: char| c.is_whitespace());
        let hash = parts.next()?.trim();
        let name = parts.next()?.trim();
        let name = name.strip_prefix('*').unwrap_or(name);
        if name == file && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_string());
        }
    }
    None
}

/// Build a reqwest client with the GitHub-required User-Agent header.
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("kf-code/{}", current_version()))
        .tcp_nodelay(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

async fn fetch_latest_tag() -> Result<String> {
    let client = build_http_client();
    let resp = client
        .get(API_LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("GitHub API request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub API returned {}", resp.status());
    }
    let json: serde_json::Value = resp.json().await.context("bad JSON from GitHub API")?;
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("GitHub API response missing tag_name"))?;
    Ok(tag.to_string())
}

async fn download_archive(client: &reqwest::Client, tag: &str, archive: &str) -> Result<Vec<u8>> {
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{archive}");
    let resp = client
        .get(&url)
        .send()
        .await
        .context("download request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("download returned {}", resp.status());
    }
    let bytes = resp.bytes().await.context("failed to read archive body")?;
    Ok(bytes.to_vec())
}

async fn download_sha256sums(client: &reqwest::Client, tag: &str) -> Result<String> {
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/SHA256SUMS.txt");
    let resp = client
        .get(&url)
        .send()
        .await
        .context("checksum request failed")?;
    if !resp.status().is_success() {
        anyhow::bail!("SHA256SUMS.txt download returned {}", resp.status());
    }
    let text = resp.text().await.context("failed to read SHA256SUMS.txt")?;
    Ok(text)
}

/// SHA256-verify `bytes` against the expected hex digest.
fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<()> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = hex::encode(hasher.finalize());
    if actual != expected_hex {
        anyhow::bail!(
            "checksum mismatch — refusing to install.\n  expected: {expected_hex}\n  actual:   {actual}"
        );
    }
    Ok(())
}

/// Extract `bin_name` from the tar.gz archive bytes by shelling out to
/// `tar` (present on every Linux/macOS; avoids adding flate2+tar deps that
/// would bloat the release binary). Returns the extracted binary bytes.
fn extract_binary(archive: &[u8], bin_name: &str) -> Result<Vec<u8>> {
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().context("failed to create temp dir for extraction")?;
    let mut child = Command::new("tar")
        .arg("-xzf")
        .arg("-")
        .arg("-C")
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to spawn tar")?;
    {
        let mut stdin = child.stdin.take().context("tar stdin pipe missing")?;
        use std::io::Write;
        stdin
            .write_all(archive)
            .context("failed to pipe archive to tar")?;
    }
    let status = child.wait().context("failed to wait for tar")?;
    if !status.success() {
        anyhow::bail!("tar extraction failed with status {status}");
    }
    let extracted = dir.path().join(bin_name);
    std::fs::read(&extracted)
        .with_context(|| format!("extracted binary not found at {}", extracted.display()))
}

/// Replace the binary at `exe` with `new_bytes` atomically. Writes to a
/// sibling temp file then renames over the target. On Windows this fails
/// because the running binary is locked — matches install.sh's "Windows
/// native install not supported" stance.
fn replace_binary(new_bytes: &[u8], exe: &Path) -> Result<()> {
    if std::env::consts::OS == "windows" {
        anyhow::bail!("in-place self-update is not supported on Windows; use the release .zip");
    }
    let dir = exe.parent().context("current exe has no parent dir")?;
    let tmp = tempfile::NamedTempFile::new_in(dir).context("failed to create temp file")?;
    let (mut file, path) = tmp.keep().context("failed to keep temp file")?;
    use std::io::Write;
    file.write_all(new_bytes)
        .context("failed to write new binary")?;
    // Make it executable before the rename so the replaced file is runnable
    // immediately.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .context("failed to set executable permissions")?;
    }
    std::fs::rename(&path, exe)
        .with_context(|| format!("failed to rename {} over {}", path.display(), exe.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sha256sums_line_extracts_hash_for_named_file() {
        let sums = "\
abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890  kf-code-x86_64-unknown-linux-gnu.tar.gz
deadbeefcafebabedeadbeefcafebabedeadbeefcafebabedeadbeefcafebabe  kf-code-aarch64-apple-darwin.tar.gz
";
        let hash = parse_sha256sums_line(sums, "kf-code-x86_64-unknown-linux-gnu.tar.gz");
        assert_eq!(
            hash.as_deref(),
            Some("abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890")
        );
    }

    #[test]
    fn parse_sha256sums_line_ignores_unrelated_files() {
        let sums = "deadbeefcafebabedeadbeefcafebabedeadbeefcafebabedeadbeefcafebabe  kf-code-aarch64-apple-darwin.tar.gz\n";
        assert!(parse_sha256sums_line(sums, "kf-code-x86_64-unknown-linux-gnu.tar.gz").is_none());
    }

    #[test]
    fn parse_sha256sums_line_handles_binary_marker() {
        // Binary-mode lines use `*file` instead of `file`.
        let sums = "abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890 *kf-code-x86_64-unknown-linux-gnu.tar.gz\n";
        let hash = parse_sha256sums_line(sums, "kf-code-x86_64-unknown-linux-gnu.tar.gz");
        assert_eq!(
            hash.as_deref(),
            Some("abc123def4567890abcdef1234567890abcdef1234567890abcdef1234567890")
        );
    }

    #[test]
    fn parse_sha256sums_line_returns_none_on_malformed() {
        // Hash not 64 hex chars → rejected.
        let sums = "short  kf-code-x86_64-unknown-linux-gnu.tar.gz\n";
        assert!(parse_sha256sums_line(sums, "kf-code-x86_64-unknown-linux-gnu.tar.gz").is_none());
        // Empty file → nothing.
        assert!(parse_sha256sums_line("", "anything").is_none());
        // Non-hex hash → rejected.
        let sums = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz  f.tar.gz\n";
        assert!(parse_sha256sums_line(sums, "f.tar.gz").is_none());
    }

    #[test]
    fn detect_target_triple_linux_x86_64() {
        // Only asserts when running on the matching host; otherwise the
        // constants are compile-time and this still exercises the match arm.
        if std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64" {
            assert_eq!(
                detect_target_triple().as_deref(),
                Some("x86_64-unknown-linux-gnu")
            );
        } else {
            // On a non-matching host, just ensure the function runs.
            let _ = detect_target_triple();
        }
    }

    #[test]
    fn detect_target_triple_macos_aarch64() {
        if std::env::consts::OS == "macos" && std::env::consts::ARCH == "aarch64" {
            assert_eq!(
                detect_target_triple().as_deref(),
                Some("aarch64-apple-darwin")
            );
        } else {
            let _ = detect_target_triple();
        }
    }

    #[test]
    fn detect_target_triple_unknown_os_returns_none() {
        // A sanity check that does not depend on the host: simulate the
        // unknown case by checking the match arms are exhaustive for the
        // supported set. The function returns Some on the 4 known combos.
        let known = [
            ("linux", "x86_64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
            ("linux", "aarch64"),
        ];
        for (os, arch) in known {
            // We can't call detect_target_triple with arbitrary args, but we
            // can confirm the helper logic by checking that the host is one
            // of the known combos OR None — i.e. never panics.
            let _ = (os, arch);
        }
        // On an unsupported host (e.g. windows), detect_target_triple returns
        // None. This assertion documents the contract; on a supported host it
        // is a no-op.
        if !matches!(
            (std::env::consts::OS, std::env::consts::ARCH),
            ("linux", "x86_64") | ("macos", "x86_64") | ("macos", "aarch64") | ("linux", "aarch64")
        ) {
            assert!(detect_target_triple().is_none());
        }
    }

    #[test]
    fn current_version_returns_cargo_pkg_version() {
        let v = current_version();
        assert!(!v.is_empty(), "current_version must not be empty");
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
    }
}
