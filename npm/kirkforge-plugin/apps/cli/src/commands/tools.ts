import type { Command } from "commander";

export function registerTools(program: Command): void {
  program
    .command("tools")
    .description("List registered verification tools")
    .action(async () => {
      console.log("KirkForge Native Lint Engines (internal, always available):");
      console.log("  JS/TS:  tool-lint-ts (29 rules)");
      console.log("  Python: tool-lint-py (34 rules)");
      console.log("  Shell:  tool-lint-sh (9 rules)");
      console.log("  C/C++:  tool-lint-c (10 rules)");
      console.log("  Rust:   tool-lint-rs (8 rules)");
      console.log("  Go:     tool-lint-go (7 rules)");
      console.log("  SQL:    tool-lint-sql (6 rules)");
      console.log("");
      console.log("Type Checkers (external, required on PATH):");
      console.log("  JS/TS:  tsc");
      console.log("  Python: pyright");
      console.log("");
      console.log("Shared Tools:");
      console.log("  gitnexus (git diff change tracking)");
      console.log("  graphify (import graph analysis, TS only)");
    });
}
