import { createHash, randomBytes } from "node:crypto";
import {
  mkdirSync,
  writeFileSync as writeFileSyncRaw,
  renameSync,
  existsSync,
  readFileSync,
  lstatSync,
  readlinkSync,
  openSync,
  fsyncSync,
  closeSync,
} from "node:fs";
import { basename, dirname, resolve, relative, isAbsolute, sep } from "node:path";

// ── Constants ──────────────────────────────────────────────────────────────

export const MAX_ARTIFACT_BYTES = 1024 * 1024; // 1 MB

export const BLOCKED_DOTFILES =
  /^\.env\b|\.npmrc\b|\.ssh\b|\.gitignore\b|\.git\b|\.htaccess\b|\.htpasswd\b|\.prettierrc\b|\.eslintrc\b|\.dockerignore\b|\.editorconfig\b|\.python-version\b|\.tool-versions\b/i;

/** IDE config directories that are acceptable to emit through artifacts. */
const ALLOWED_HIDDEN_SEGMENTS = new Set([".vscode", ".idea"]);

// ── Hash helpers ────────────────────────────────────────────────────────────

export function sha256Of(content: string): string {
  return createHash("sha256").update(content, "utf-8").digest("hex");
}

export function sha256OfRaw(buf: Buffer): string {
  return createHash("sha256").update(buf).digest("hex");
}

// ── Path containment ────────────────────────────────────────────────────────

/**
 * Returns true if `fullPath` is strictly inside `cwd`.
 * Rejects absolute paths, `..` escapes, and prefix-collision siblings.
 */
export function isInsideCwd(fullPath: string, cwd: string): boolean {
  const rel = relative(cwd, fullPath);
  if (
    rel === "" ||
    rel === ".." ||
    rel.startsWith(".." + sep) ||
    rel.startsWith("../") ||
    isAbsolute(rel)
  )
    return false;
  return true;
}

/**
 * Centralised safe relative path from user / CLI input.
 * Rejects: absolute paths, `..` segments, empty strings, hidden segments
 * (when `allowHidden` is false).
 */
export function safeRelativePath(
  cwd: string,
  userPath: string,
  opts?: { allowHidden?: boolean },
): string | null {
  if (!userPath || userPath.trim().length === 0) return null;
  const resolved = resolve(cwd, userPath);
  const rel = relative(cwd, resolved);
  if (
    rel === "" ||
    rel === ".." ||
    rel.startsWith(".." + sep) ||
    rel.startsWith("../") ||
    isAbsolute(rel)
  )
    return null;

  // Walk segments — reject hidden entries at any depth
  if (!opts?.allowHidden) {
    const segments = rel.split(sep);
    for (const seg of segments) {
      if (seg.startsWith(".") && seg !== "." && seg !== "..") return null;
    }
  }
  return rel;
}

// ── Content safety ──────────────────────────────────────────────────────────

export function isBinaryLikeContent(content: string): boolean {
  const sample = content.slice(0, 8192);
  if (sample.length === 0) return false;
  let nonPrintable = 0;
  for (let i = 0; i < sample.length; i++) {
    const cc = sample.charCodeAt(i);
    if (cc < 32 && cc !== 9 && cc !== 10 && cc !== 13) nonPrintable++;
  }
  return nonPrintable / sample.length > 0.3;
}

// ── Path helpers ────────────────────────────────────────────────────────────

export function isAbsolutePath(p: string): boolean {
  return p.startsWith("/") || /^[A-Za-z]:\\/.test(p);
}

export function extractExtension(filePath: string): string {
  const base = basename(filePath);
  const dotIndex = base.lastIndexOf(".");
  return dotIndex > 0 ? base.slice(dotIndex).toLowerCase() : "";
}

export function hasHiddenSegment(filePath: string): boolean {
  const parts = filePath.split(/[/\\]+/);
  for (const part of parts) {
    if (part.startsWith(".") && part !== "." && part !== "..") {
      if (!ALLOWED_HIDDEN_SEGMENTS.has(part)) {
        return true;
      }
    }
  }
  return false;
}

// ── Symlink guards ──────────────────────────────────────────────────────────

/**
 * Walk each path segment and fail if any intermediate directory or file
 * is a symlink whose target escapes `cwd`.
 */
export function segmentsHaveEscapingSymlink(filePath: string, cwd: string): boolean {
  const segments = filePath.split(sep).filter(Boolean);
  for (let i = 1; i <= segments.length; i++) {
    const partial = `${sep}${segments.slice(0, i).join(sep)}`;
    try {
      const st = lstatSync(partial);
      if (st.isSymbolicLink()) {
        const resolvedTarget = resolve(dirname(partial), readlinkSync(partial));
        if (!isInsideCwd(resolvedTarget, cwd)) return true;
      }
    } catch {
      // ENOENT on intermediate segments is normal — target may not exist yet
    }
  }
  return false;
}

/** True when final path exists and is a symlink (never safe to write through). */
export function finalFileIsSymlink(filePath: string): boolean {
  try {
    const st = lstatSync(filePath);
    return st.isSymbolicLink();
  } catch {
    return false;
  }
}

// ── Artifact allow/deny ─────────────────────────────────────────────────────

export interface ArtifactRecord {
  filePath: string;
  content: string;
}

export interface TaskProfileLike {
  language?: string;
  allowedExtensions?: string[];
  forbiddenExtensions?: string[];
  writePolicy?: {
    allowOverwrite?: boolean;
    denyPaths?: string[];
  };
}

/**
 * Return a rejection reason string if `art` should be blocked,
 * or `null` if the artifact is allowed by policy.
 */
export function disallowedArtifact(art: ArtifactRecord, profile?: TaskProfileLike): string | null {
  // Absolute paths are never safe for artifact writes
  if (isAbsolutePath(art.filePath)) {
    return `absolute path "${art.filePath}" is not allowed`;
  }

  const base = basename(art.filePath);

  // Block ALL hidden dotfiles (not just known sensitive ones in artifact context)
  if (base.startsWith(".")) {
    return `hidden dotfile "${base}" is not allowed (dotfiles are default-deny)`;
  }

  // Block hidden directory segments (with IDE config exceptions)
  if (hasHiddenSegment(art.filePath)) {
    return `hidden directory segment in path "${art.filePath}" is not allowed`;
  }

  const ext = extractExtension(art.filePath);

  // Forbidden extensions
  if (ext !== "" && profile?.forbiddenExtensions && profile.forbiddenExtensions.includes(ext)) {
    const lang = profile?.language ?? "task";
    return `${lang} task cannot emit "${art.filePath}" (forbidden extension ${ext})`;
  }

  // Allowed extensions (when the list is defined and non-empty)
  if (profile?.allowedExtensions && profile.allowedExtensions.length > 0) {
    if (ext === "") {
      return `${profile.language ?? "task"} task emitted "${art.filePath}" — no-extension files not allowed`;
    }
    if (!profile.allowedExtensions.includes(ext)) {
      return `${profile.language ?? "task"} task emitted "${art.filePath}" with unexpected extension ${ext}`;
    }
  }

  return null;
}

// ── Atomic write ────────────────────────────────────────────────────────────

export interface WriteResult {
  filePath: string;
  bytes: number;
  ok: boolean;
  blocked?: string;
  warning?: string;
  sha256?: string;
  beforeHash?: string | null;
  existed?: boolean;
}

/**
 * Write `artifacts` into `cwd` with all safety checks applied.
 * Uses temp-file + rename for atomic writes.
 */
export function writeArtifacts(
  artifacts: ArtifactRecord[],
  cwd: string,
  profile?: TaskProfileLike,
): WriteResult[] {
  const results: WriteResult[] = [];
  for (const art of artifacts) {
    const fullPath = resolve(cwd, art.filePath);

    // 1. Sandbox containment
    if (!isInsideCwd(fullPath, cwd)) {
      results.push({
        filePath: art.filePath,
        bytes: 0,
        ok: false,
        blocked: `resolved path escapes sandbox: ${art.filePath}`,
      });
      continue;
    }

    // 2. Extension / dotfile policy
    const rejection = disallowedArtifact(art, profile);
    if (rejection) {
      results.push({ filePath: art.filePath, bytes: 0, ok: false, blocked: rejection });
      continue;
    }

    // 3. Symlink traversal guard
    if (segmentsHaveEscapingSymlink(fullPath, cwd)) {
      results.push({
        filePath: art.filePath,
        bytes: 0,
        ok: false,
        blocked: `symlink escape detected in path: ${art.filePath}`,
      });
      continue;
    }

    // 4. Final symlink guard
    if (finalFileIsSymlink(fullPath)) {
      results.push({
        filePath: art.filePath,
        bytes: 0,
        ok: false,
        blocked: `final path is symlink — writes would follow link outside sandbox: ${art.filePath}`,
      });
      continue;
    }

    // 5. Size limit
    const byteLength = Buffer.byteLength(art.content, "utf-8");
    if (byteLength > MAX_ARTIFACT_BYTES) {
      results.push({
        filePath: art.filePath,
        bytes: 0,
        ok: false,
        blocked: `artifact exceeds maximum size (${byteLength} bytes > ${MAX_ARTIFACT_BYTES} bytes)`,
      });
      continue;
    }

    // 6. Binary check
    if (isBinaryLikeContent(art.content)) {
      results.push({
        filePath: art.filePath,
        bytes: 0,
        ok: false,
        blocked: `artifact rejected: content appears binary-like (>30% non-printable characters in first 8 KB)`,
      });
      continue;
    }

    // 7. Hash & existence
    const hash = sha256Of(art.content);
    const existed = existsSync(fullPath);

    // 8. Write policy — overwrite deny
    if (existed && profile?.writePolicy?.allowOverwrite !== true) {
      results.push({
        filePath: art.filePath,
        bytes: 0,
        ok: false,
        blocked: `overwrite denied (allowOverwrite not enabled in writePolicy): ${art.filePath}`,
      });
      continue;
    }

    // 9. Write policy — denyPaths
    if (profile?.writePolicy?.denyPaths) {
      const relPath = relative(cwd, fullPath);
      if (profile.writePolicy.denyPaths.some((d) => relPath === d || relPath.startsWith(d + "/"))) {
        results.push({
          filePath: art.filePath,
          bytes: 0,
          ok: false,
          blocked: `path denied by writePolicy: ${art.filePath}`,
        });
        continue;
      }
    }

    // 8b. Before-hash (read after policy checks, guarded against throw)
    let beforeHash: string | null = null;
    if (existed) {
      try {
        beforeHash = sha256OfRaw(readFileSync(fullPath));
      } catch {
        results.push({
          filePath: art.filePath,
          bytes: 0,
          ok: false,
          blocked: `cannot read existing file for beforeHash: ${art.filePath}`,
        });
        continue;
      }
    }

    // 10. Atomic write
    try {
      mkdirSync(dirname(fullPath), { recursive: true });
      const tmpPath = fullPath + ".tmp." + Date.now() + "." + randomBytes(4).toString("hex");
      writeFileSyncRaw(tmpPath, art.content, "utf-8");
      // fsync for durability (important for daemon/bot)
      try {
        const fd = openSync(tmpPath, "r");
        try {
          fsyncSync(fd);
        } finally {
          closeSync(fd);
        }
      } catch {
        /* best-effort */
      }
      renameSync(tmpPath, fullPath);
      results.push({
        filePath: relative(cwd, fullPath),
        bytes: byteLength,
        ok: true,
        sha256: hash,
        beforeHash,
        existed,
      });
    } catch (e) {
      results.push({
        filePath: art.filePath,
        bytes: 0,
        ok: false,
        blocked: `write error: ${e instanceof Error ? e.message : String(e)}`,
      });
    }
  }
  return results;
}
