import { LintEngine } from "@kirkforge/tool-lint-core";
import type { LintEngineOptions } from "@kirkforge/tool-lint-core";
import type { LintRule } from "@kirkforge/tool-lint-core";

const SH_EXTS = new Set([".sh", ".bash", ".zsh"]);

const shellRules: LintRule[] = [
  {
    id: "no-unquoted-vars",
    category: "safety",
    severity: "high",
    pattern: /\$\{?\w+\}?(?![^"]*")/g,
    message:
      'Unquoted variable expansion — wrap in double quotes using echo \\"$var\\" or \\"${var}\\" to prevent word splitting and globbing',
  },
  {
    id: "no-backticks",
    category: "safety",
    severity: "med",
    pattern: /`[^`]+`/g,
    message:
      "Backtick command substitution is legacy — use $(command) instead; nests correctly and is more readable",
  },
  {
    id: "no-eval",
    category: "safety",
    severity: "critical",
    pattern: /\beval\s+/g,
    message:
      "eval executes arbitrary strings as shell code — there is no safe use; restructure to avoid dynamic command construction",
  },
  {
    id: "no-sudo",
    category: "safety",
    severity: "med",
    pattern: /\bsudo\s+/g,
    message:
      "sudo in scripts breaks non-interactive execution — run the entire script with the needed permissions, or use a privileged wrapper",
  },
  {
    id: "no-curl-bash-pipe",
    category: "safety",
    severity: "critical",
    pattern: /curl\s+\S+\s*\|\s*(?:ba)?sh/g,
    message:
      "curl | bash runs untrusted remote code — download the script, inspect it, checksum-verify, then execute locally",
  },
  {
    id: "no-unset-vars",
    category: "correct",
    severity: "med",
    pattern: /\$\{\w+:?\}/g,
    message:
      "Unset variable may cause silent failures — use ${var:?} to exit with error, or set -u at the top of the script",
  },

  {
    id: "no-cd-fail",
    category: "correct",
    severity: "med",
    pattern: /\bcd\s+(?!.*\|\||.*&&)/g,
    message:
      "cd can fail silently (missing dir, permission denied) — use cd /path || exit 1, or set -e at script start",
  },
  {
    id: "no-rm-rf-star",
    category: "safety",
    severity: "critical",
    pattern: /\brm\s+-rf?\s+\*/g,
    message:
      "rm -rf * is a destructive wildcard — specify exact paths; if you must delete a directory path, use rm -rf /exact/target",
  },
  {
    id: "no-hardcoded-path",
    category: "maintain",
    severity: "low",
    pattern: /(?<!#)\/usr\/local\/bin\/\w+/g,
    message:
      "Hardcoded binary path is not portable — use command -v <tool> or an environment variable to locate the binary",
  },
];

export function createShLintEngine(opts: LintEngineOptions): LintEngine {
  const engine = new LintEngine({ ...opts, extensions: SH_EXTS });
  engine.addRules(shellRules);
  return engine;
}
