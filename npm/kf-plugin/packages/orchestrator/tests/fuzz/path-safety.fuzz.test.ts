/**
 * Fuzz tests for path-safety and artifact-mode protocol parsers.
 * These test edge cases, malicious inputs, and boundary conditions
 * that regular unit tests might miss.
 */
import { describe, it, expect } from "vitest";
import {
  isInsideCwd,
  safeRelativePath,
  isBinaryLikeContent,
  sha256Of,
  MAX_ARTIFACT_BYTES,
} from "../../src/path-safety.js";
import { parseJsonlArtifacts, parseArtifacts } from "../../src/artifact-mode.js";

// ── Helpers ────────────────────────────────────────────────────────────────

function randomString(len: number): string {
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789./\\\x00\n\r\t";
  let result = "";
  for (let i = 0; i < len; i++) {
    result += chars[Math.floor(Math.random() * chars.length)];
  }
  return result;
}

// ── isInsideCwd fuzz ───────────────────────────────────────────────────────

describe("isInsideCwd fuzz", () => {
  it("never returns true for absolute paths outside cwd", () => {
    const cwd = "/home/user/project";
    const outside = ["/etc/passwd", "/root/.ssh", "/tmp/../../etc/shadow", "/"];
    for (const path of outside) {
      expect(isInsideCwd(path, cwd)).toBe(false);
    }
  });

  it("never returns true for traversal escapes", () => {
    const cwd = "/home/user/project";
    const escapes = [
      "../etc/passwd",
      "../../root/.ssh",
      "subdir/../../../etc",
      "..",
      "../..",
      "./../../etc",
    ];
    for (const path of escapes) {
      expect(isInsideCwd("/home/user/project/" + path.replace(/\.\.\//g, "../"), cwd)).toBe(false);
    }
  });

  it("handles empty and weird paths gracefully", () => {
    const cwd = "/tmp/test";
    expect(isInsideCwd("", cwd)).toBe(false);
    expect(isInsideCwd("/tmp/test", cwd)).toBe(false); // cwd itself
  });

  it("prefix-collision paths are rejected", () => {
    const cwd = "/home/user/project";
    // "/home/user/project-other" should NOT be inside "/home/user/project"
    expect(isInsideCwd("/home/user/project-other/file.txt", cwd)).toBe(false);
  });
});

// ── safeRelativePath fuzz ───────────────────────────────────────────────────

describe("safeRelativePath fuzz", () => {
  it("rejects null/empty/whitespace", () => {
    const cwd = "/tmp/test";
    expect(safeRelativePath(cwd, "")).toBeNull();
    expect(safeRelativePath(cwd, "   ")).toBeNull();
    expect(safeRelativePath(cwd, "\n\t")).toBeNull();
  });

  it("rejects absolute paths", () => {
    const cwd = "/tmp/test";
    expect(safeRelativePath(cwd, "/etc/passwd")).toBeNull();
    expect(safeRelativePath(cwd, "/tmp/other")).toBeNull();
  });

  it("rejects hidden segments", () => {
    const cwd = "/tmp/test";
    expect(safeRelativePath(cwd, ".env")).toBeNull();
    expect(safeRelativePath(cwd, "subdir/.git/config")).toBeNull();
    expect(safeRelativePath(cwd, ".ssh/id_rsa")).toBeNull();
  });

  it("allows .vscode and .idea hidden segments with allowHidden", () => {
    const cwd = "/tmp/test";
    expect(safeRelativePath(cwd, ".vscode/settings.json", { allowHidden: true })).not.toBeNull();
    expect(safeRelativePath(cwd, ".idea/workspace.xml", { allowHidden: true })).not.toBeNull();
  });

  it("rejects traversal attempts", () => {
    const cwd = "/tmp/test";
    expect(safeRelativePath(cwd, "../etc/passwd")).toBeNull();
    expect(safeRelativePath(cwd, "foo/../../bar")).toBeNull();
  });

  it("random fuzz: never returns a traversal path", () => {
    const cwd = "/tmp/fuzz-test";
    for (let i = 0; i < 500; i++) {
      const input = randomString(Math.floor(Math.random() * 200));
      const result = safeRelativePath(cwd, input);
      if (result !== null) {
        // ".." as a standalone path component is rejected, but ".." inside
        // a filename (e.g. "some..thing") is valid on most filesystems.
        const segments = result.split("/");
        expect(segments).not.toContain("..");
        expect(result).not.toMatch(/^\//);
      }
    }
  });
});

// ── isBinaryLikeContent fuzz ────────────────────────────────────────────────

describe("isBinaryLikeContent fuzz", () => {
  it("empty string is not binary", () => {
    expect(isBinaryLikeContent("")).toBe(false);
  });

  it("normal text is not binary", () => {
    expect(isBinaryLikeContent("hello world\nprint('ok')")).toBe(false);
    expect(isBinaryLikeContent("function foo() { return 42; }")).toBe(false);
    expect(isBinaryLikeContent("SELECT * FROM users;")).toBe(false);
  });

  it("highly non-printable content is binary", () => {
    const binary = Buffer.alloc(1000, 0);
    expect(isBinaryLikeContent(binary.toString("binary"))).toBe(true);
  });

  it("handles Unicode text gracefully", () => {
    expect(isBinaryLikeContent("こんにちは世界")).toBe(false);
    expect(isBinaryLikeContent("🚀✨ test")).toBe(false);
  });

  it("random fuzz: never throws", () => {
    for (let i = 0; i < 500; i++) {
      const input = randomString(Math.floor(Math.random() * 1000));
      expect(() => isBinaryLikeContent(input)).not.toThrow();
    }
  });
});

// ── sha256Of fuzz ──────────────────────────────────────────────────────────

describe("sha256Of fuzz", () => {
  it("deterministic: same input → same hash", () => {
    const input = "hello world";
    expect(sha256Of(input)).toBe(sha256Of(input));
  });

  it("different inputs → different hashes", () => {
    expect(sha256Of("hello")).not.toBe(sha256Of("world"));
  });

  it("empty string produces a valid 64-char hex hash", () => {
    const hash = sha256Of("");
    expect(hash).toHaveLength(64);
    expect(hash).toMatch(/^[a-f0-9]{64}$/);
  });

  it("handles large inputs without throwing", () => {
    const large = "x".repeat(MAX_ARTIFACT_BYTES);
    expect(() => sha256Of(large)).not.toThrow();
    expect(sha256Of(large)).toHaveLength(64);
  });
});

// ── parseJsonlArtifacts fuzz ───────────────────────────────────────────────

describe("parseJsonlArtifacts fuzz", () => {
  it("empty string returns no artifacts", () => {
    const result = parseJsonlArtifacts("");
    expect(result.artifacts).toHaveLength(0);
    // With B12 fix, empty output is also non-strict (caught downstream)
    expect(result.strictTermination).toBe(false);
  });

  it("non-JSON garbage returns no artifacts", () => {
    const result = parseJsonlArtifacts("not json\nstill not json\n### FILE: foo.txt");
    expect(result.artifacts).toHaveLength(0);
  });

  it("jsonl lines without type file_write are ignored", () => {
    const result = parseJsonlArtifacts('{"type":"other","path":"x"}\n{"foo":"bar"}');
    expect(result.artifacts).toHaveLength(0);
  });

  it("malformed JSON on a line is skipped", () => {
    const result = parseJsonlArtifacts('{"type":"file_write","path":"x"\nvalid line after');
    expect(result.artifacts).toHaveLength(0);
  });

  it("marker protocol not active by default", () => {
    // Without ALLOW_MARKER_ARTIFACT_FALLBACK=1, markers should be ignored
    const result = parseJsonlArtifacts("### FILE: test.txt\ncontent\n### END");
    expect(result.artifacts).toHaveLength(0);
  });

  it("random fuzz: never throws", () => {
    for (let i = 0; i < 500; i++) {
      const input = randomString(Math.floor(Math.random() * 500));
      expect(() => parseJsonlArtifacts(input)).not.toThrow();
    }
  });

  it("random fuzz: strictTermination is always boolean", () => {
    for (let i = 0; i < 500; i++) {
      const input = randomString(Math.floor(Math.random() * 500));
      const result = parseJsonlArtifacts(input);
      expect(typeof result.strictTermination).toBe("boolean");
    }
  });
});

// ── parseArtifacts (marker) fuzz ────────────────────────────────────────────

describe("parseArtifacts fuzz", () => {
  it("random fuzz: never throws", () => {
    for (let i = 0; i < 500; i++) {
      const input = randomString(Math.floor(Math.random() * 500));
      expect(() => parseArtifacts(input)).not.toThrow();
    }
  });

  it("warns on unterminated marker", () => {
    const result = parseArtifacts("### FILE: test.txt\nsome content");
    expect(result.strictTermination).toBe(false);
    expect(result.warnings.length).toBeGreaterThan(0);
  });

  it("handles content containing marker-like patterns", () => {
    // Lines matching ### FILE: inside content are consumed as file markers,
    // not treated as content. This is expected marker-parser behavior.
    // The collision warning fires for content between markers that matches the pattern
    // (e.g. when content is pushed before the next marker is consumed).
    const result = parseArtifacts("### FILE: a.txt\nreal content\n### FILE: b.txt\nmore\n### END");
    // Both files extracted; second ### FILE: starts a new file
    expect(result.artifacts.length).toBeGreaterThanOrEqual(2);
  });
});
