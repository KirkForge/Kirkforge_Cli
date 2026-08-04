import type { TaskLanguage, VerifierPolicy } from "@kirkforge/correction-core";

export type { TaskLanguage } from "@kirkforge/correction-core";
export type { VerifierPolicy } from "@kirkforge/correction-core";

export interface StructuredCheckCommand {
  command: string;
  args: string[];
  appendFiles?: boolean;
}

export interface EmissionSchema {
  language: TaskLanguage;
  defaultFile: string;
  fenceLanguages: string[];
  checkCommand: string;
  structuredCheck?: StructuredCheckCommand;
  promptHint: string;
  allowedExtensions: string[];
  forbiddenExtensions: string[];
  verifierPolicy: VerifierPolicy;
  validatorRequired?: boolean;
  writePolicy?: {
    allowOverwrite?: boolean;
    denyPaths?: string[];
  };
}

export type TaskProfile = EmissionSchema;

const PROFILES: Record<TaskLanguage, TaskProfile> = {
  typescript: {
    language: "typescript",
    defaultFile: "output.ts",
    fenceLanguages: ["typescript", "ts"],
    checkCommand: "npx tsc --noEmit",
    structuredCheck: { command: "npx", args: ["tsc", "--noEmit"], appendFiles: false },
    promptHint: "Emit TypeScript files. Prefer .ts paths.",
    allowedExtensions: [".ts", ".tsx", ".json", ".css", ".html", ".txt"],
    forbiddenExtensions: [".py", ".rs", ".go", ".sh"],
    verifierPolicy: { required: ["lint", "types", "security"], advisory: ["graph"] },
  },
  javascript: {
    language: "javascript",
    defaultFile: "output.js",
    fenceLanguages: ["javascript", "js"],
    checkCommand: "node --check",
    structuredCheck: { command: "node", args: ["--check"], appendFiles: true },
    promptHint: "Emit JavaScript files. Prefer .js paths.",
    allowedExtensions: [".js", ".jsx", ".json", ".css", ".html", ".txt"],
    forbiddenExtensions: [".py", ".rs", ".go"],
    verifierPolicy: { required: ["lint", "security"], advisory: ["types", "graph"] },
  },
  python: {
    language: "python",
    defaultFile: "solution.py",
    fenceLanguages: ["python", "py"],
    checkCommand: "python3 -m py_compile",
    structuredCheck: { command: "python3", args: ["-m", "py_compile"], appendFiles: true },
    promptHint: "Emit Python files. Prefer .py paths.",
    allowedExtensions: [".py", ".txt", ".toml", ".cfg"],
    forbiddenExtensions: [".ts", ".tsx", ".js", ".jsx", ".d.ts"],
    verifierPolicy: { required: ["lint", "types"], advisory: ["security", "graph"] },
  },
  shell: {
    language: "shell",
    validatorRequired: true,
    defaultFile: "solution.sh",
    fenceLanguages: ["bash", "sh", "shell"],
    checkCommand: "bash -n",
    structuredCheck: { command: "bash", args: ["-n"], appendFiles: true },
    promptHint: "Emit POSIX shell files. Prefer .sh paths. Install shellcheck for best results.",
    allowedExtensions: [".sh", ".bash", ".txt"],
    forbiddenExtensions: [".ts", ".tsx", ".js", ".py", ".rs"],
    verifierPolicy: { required: ["security", "lint"], advisory: ["types", "graph"] },
  },
  cpp: {
    language: "cpp",
    validatorRequired: true,
    defaultFile: "solution.cpp",
    fenceLanguages: ["cpp", "c++"],
    checkCommand: "g++ -fsyntax-only",
    structuredCheck: { command: "g++", args: ["-fsyntax-only"], appendFiles: true },
    promptHint: "Emit C++ files. Prefer .cpp paths.",
    allowedExtensions: [".cpp", ".cc", ".cxx", ".h", ".hpp", ".txt"],
    forbiddenExtensions: [".ts", ".tsx", ".py"],
    verifierPolicy: { required: [], advisory: ["lint", "types", "security", "graph"] },
  },
  c: {
    language: "c",
    validatorRequired: true,
    defaultFile: "solution.c",
    fenceLanguages: ["c"],
    checkCommand: "gcc -fsyntax-only",
    structuredCheck: { command: "gcc", args: ["-fsyntax-only"], appendFiles: true },
    promptHint: "Emit C files. Prefer .c paths.",
    allowedExtensions: [".c", ".h", ".txt"],
    forbiddenExtensions: [".ts", ".tsx", ".py"],
    verifierPolicy: { required: [], advisory: ["lint", "types", "security", "graph"] },
  },
  rust: {
    language: "rust",
    validatorRequired: true,
    defaultFile: "solution.rs",
    fenceLanguages: ["rust", "rs"],
    checkCommand: "rustc --emit=metadata",
    structuredCheck: { command: "rustc", args: ["--emit=metadata"], appendFiles: true },
    promptHint: "Emit Rust files. Prefer .rs paths.",
    allowedExtensions: [".rs", ".toml", ".txt"],
    forbiddenExtensions: [".ts", ".tsx", ".py"],
    verifierPolicy: { required: [], advisory: ["lint", "types", "security", "graph"] },
  },
  go: {
    language: "go",
    validatorRequired: true,
    defaultFile: "main.go",
    fenceLanguages: ["go"],
    checkCommand: "go vet ./...",
    structuredCheck: { command: "go", args: ["vet", "./..."], appendFiles: false },
    promptHint: "Emit Go files. Prefer .go paths.",
    allowedExtensions: [".go", ".mod", ".sum", ".txt"],
    forbiddenExtensions: [".ts", ".tsx", ".py"],
    verifierPolicy: { required: [], advisory: ["lint", "types", "security", "graph"] },
  },
  sql: {
    language: "sql",
    validatorRequired: true,
    defaultFile: "query.sql",
    fenceLanguages: ["sql"],
    checkCommand: "",
    promptHint: "Emit SQL files. Prefer .sql paths.",
    allowedExtensions: [".sql", ".txt"],
    forbiddenExtensions: [".ts", ".tsx", ".py"],
    verifierPolicy: { required: [], advisory: ["lint", "types", "security", "graph"] },
  },
  text: {
    language: "text",
    validatorRequired: true,
    defaultFile: "answer.txt",
    fenceLanguages: ["text"],
    checkCommand: "",
    promptHint: "Emit .txt or .md. Other extensions require explicit --language or --validator.",
    allowedExtensions: [".txt", ".md"],
    forbiddenExtensions: [
      ".ts",
      ".tsx",
      ".js",
      ".jsx",
      ".py",
      ".rs",
      ".go",
      ".sh",
      ".bash",
      ".exe",
      ".dll",
      ".so",
      ".json",
      ".csv",
      ".yaml",
      ".yml",
      ".toml",
      ".xml",
      ".html",
      ".css",
    ],
    verifierPolicy: { required: [], advisory: ["lint", "types", "security", "graph"] },
    writePolicy: { allowOverwrite: false },
  },
};

const RULES: Array<{ language: TaskLanguage; pattern: RegExp }> = [
  {
    language: "python",
    pattern:
      /\b(?:python|py_compile|pytest|pandas|flask|django|cython|pip|requirements\.txt|csv|parquet|jupyter|notebook|classifier|debug.*program|broken-python|vul-flask)\b/i,
  },
  {
    language: "shell",
    pattern:
      /\b(?:bash|shell|sh script|script|bucket|aws|s3|cron|unix|linux|command line|cli command|create-bucket)\b/i,
  },
  { language: "cpp", pattern: /\b(?:c\+\+|cpp|g\+\+|clang\+\+|cmake|cpp-compatibility)\b/i },
  { language: "c", pattern: /\b(?:gcc|clang|makefile|\.c\b|c program)\b/i },
  { language: "rust", pattern: /\b(?:rust|cargo|rustc)\b/i },
  { language: "go", pattern: /\b(?:golang|go test|go\.mod)\b/i },
  { language: "sql", pattern: /\b(?:sql|sqlite|postgres|query|database|simple-sql-query)\b/i },
  { language: "javascript", pattern: /\b(?:javascript|node\.?js|node --check|\.js|jsx)\b/i },
  {
    language: "typescript",
    pattern:
      /\b(?:typescript|\bts\b|tsc|\.ts|tsx|web scraper|form-filling|server endpoint|endpoint)\b/i,
  },
];

export function detectTaskProfile(description: string): TaskProfile {
  for (const rule of RULES) {
    if (rule.pattern.test(description)) return PROFILES[rule.language]!;
  }
  return PROFILES.text!;
}

export function profileForLanguage(language: TaskLanguage): TaskProfile {
  return PROFILES[language]!;
}

export function extensionForLanguage(language: string | undefined): string {
  switch ((language ?? "").toLowerCase()) {
    case "python":
      return ".py";
    case "shell":
    case "bash":
    case "sh":
      return ".sh";
    case "cpp":
    case "c++":
      return ".cpp";
    case "c":
      return ".c";
    case "rust":
      return ".rs";
    case "go":
      return ".go";
    case "sql":
      return ".sql";
    case "javascript":
    case "js":
      return ".js";
    case "typescript":
    case "ts":
      return ".ts";
    default:
      return ".txt";
  }
}
