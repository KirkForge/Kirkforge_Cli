// Curated rename tables for Python and TypeScript / JavaScript imports.
// Loaded from the data/*.json files at module load. Keep these in sync with
// src/data/{python,typescript}-renames.json.

import pythonRenames from "./data/python-renames.json" with { type: "json" };
import typescriptRenames from "./data/typescript-renames.json" with { type: "json" };

export interface RenameEntry {
  replacedBy: string;
  deprecatedSince: string;
  reason: string;
}

// Strip the "$schema" and "_comment" meta fields before exposing the tables.
function stripMeta<T extends Record<string, RenameEntry>>(raw: Record<string, unknown>): T {
  const out: Record<string, RenameEntry> = {};
  for (const [k, v] of Object.entries(raw)) {
    if (k.startsWith("$") || k.startsWith("_")) continue;
    if (
      v &&
      typeof v === "object" &&
      "replacedBy" in v &&
      "deprecatedSince" in v &&
      "reason" in v
    ) {
      out[k] = v as RenameEntry;
    }
  }
  return out as T;
}

export const BUILTIN_PYTHON_RENAMES: Record<string, RenameEntry> = stripMeta(pythonRenames);
export const BUILTIN_TYPESCRIPT_RENAMES: Record<string, RenameEntry> = stripMeta(typescriptRenames);
