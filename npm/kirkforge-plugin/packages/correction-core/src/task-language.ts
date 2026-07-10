export type TaskLanguage =
  | "typescript"
  | "javascript"
  | "python"
  | "shell"
  | "cpp"
  | "c"
  | "rust"
  | "go"
  | "sql"
  | "text";

export interface TaskProfile {
  language: TaskLanguage;
  defaultFile: string;
  fenceLanguages: string[];
  checkCommand: string;
  promptHint: string;
  allowedExtensions: string[];
  forbiddenExtensions: string[];
}
