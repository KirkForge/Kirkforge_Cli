import type { Command } from "commander";
import { createBootstrap } from "../bootstrap.js";
import { ALL_MODES, exitError } from "../shared.js";

export function registerRun(program: Command): void {
  program
    .command("run")
    .description("Run a task with the correction loop (accept/correct/escalate)")
    .argument("<description>", "Task description")
    .option("--mode <mode>", `Delegation mode: ${ALL_MODES.join(", ")}`)
    .option("--provider <key>", "Provider key from config")
    .option("--max-corrections <n>", "Maximum correction attempts", "2")
    .option("--max-cost <n>", "Maximum cost budget in USD")
    .option("--context <text>", "Additional context for the task")
    .option("--file <paths...>", "Target files for the task")
    .option(
      "--language <name>",
      "Pin the language profile (typescript, python, shell, ...). Skips auto-detection.",
    )
    .option(
      "--verifier-policy <json>",
      "JSON override for the profile's verifierPolicy, e.g. '{\"required\":[],\"advisory\":[]}'",
    )
    .option(
      "--validator <command>",
      "Structured validator: command name (no shell expansion, args passed separately)",
    )
    .option("--validator-args <args...>", "Arguments for structured --validator")
    .option(
      "--validator-shell <command>",
      "Raw shell validator command (unsafe: host must sandbox); exit 0 means pass",
    )
    .option("--validator-timeout-ms <n>", "Validator timeout in milliseconds", "120000")
    .option("--max-tokens <n>", "Maximum output tokens for model generation")
    .option("--temperature <n>", "Model sampling temperature (0.0-2.0)")
    .option("--json", "JSON output")
    .action(async (description, opts) => {
      if (opts.mode && !ALL_MODES.includes(opts.mode)) {
        exitError(`--mode must be one of: ${ALL_MODES.join(", ")}`, opts.json);
      }
      const maxCorrections = parseInt(opts.maxCorrections ?? "2", 10);
      const maxCost = opts.maxCost ? parseFloat(opts.maxCost) : undefined;
      const validatorTimeoutMs = parseInt(opts.validatorTimeoutMs ?? "120000", 10);

      if (Number.isNaN(maxCorrections) || maxCorrections < 0 || !Number.isInteger(maxCorrections)) {
        exitError("--max-corrections must be a non-negative integer", opts.json);
      }
      if (
        opts.maxCost !== undefined &&
        maxCost !== undefined &&
        (Number.isNaN(maxCost) || maxCost < 0)
      ) {
        exitError("--max-cost must be a non-negative number", opts.json);
      }
      if (opts.validator && (Number.isNaN(validatorTimeoutMs) || validatorTimeoutMs <= 0)) {
        exitError("--validator-timeout-ms must be a positive integer", opts.json);
      }

      // Raw shell validator is gated behind ALLOW_UNSAFE_SHELL_VALIDATOR — enterprise policy
      if (opts.validatorShell && process.env.ALLOW_UNSAFE_SHELL_VALIDATOR !== "true") {
        exitError(
          "--validator-shell requires ALLOW_UNSAFE_SHELL_VALIDATOR=true (unsafe: host must sandbox)",
          opts.json,
        );
      }

      const validatorConfig = opts.validatorShell
        ? { shellCommand: opts.validatorShell, timeoutMs: validatorTimeoutMs }
        : opts.validator
          ? { command: opts.validator, args: opts.validatorArgs ?? [], timeoutMs: validatorTimeoutMs }
          : undefined;

      const {
        orchestrator,
        shutdown: _shutdown,
        policyEngine: _policyEngine,
        auditLogger: _auditLogger,
      } = await createBootstrap(opts);

      let verifierPolicyOverride: { required: import("@kirkforge/correction-core").VerifierSlot[]; advisory: import("@kirkforge/correction-core").VerifierSlot[] } | undefined;
      if (opts.verifierPolicy) {
        try {
          verifierPolicyOverride = JSON.parse(opts.verifierPolicy);
        } catch (e) {
          exitError(`--verifier-policy must be valid JSON: ${e instanceof Error ? e.message : e}`, opts.json);
        }
      }

      const outcome = await orchestrator.runCorrectionLoop(
        {
          description,
          modeOverride: opts.mode,
          context: opts.context,
          files: opts.file,
          ...(opts.language ? { language: opts.language } : {}),
          ...(verifierPolicyOverride ? { verifierPolicy: verifierPolicyOverride } : {}),
        },
        { maxCorrections, maxCost, validator: validatorConfig },
      );

      if (opts.json) {
        console.log(
          JSON.stringify(
            {
              finalAction: outcome.finalAction,
              finalVerdict: outcome.finalVerdict,
              sourceOfTruth: outcome.sourceOfTruth,
              taskValidation: outcome.taskValidation,
              taskOutcome: outcome.taskOutcome,
              taskPass:
                outcome.taskValidation.status === "pass"
                  ? true
                  : outcome.taskValidation.status === "fail"
                    ? false
                    : null,
              turns: outcome.turns.map((t, i) => ({
                turn: i + 1,
                action: t.action,
                rationale: t.rationale,
                workerTokens: t.workerTokens,
                sessionTokens: t.sessionTokens,
                verification: t.packet.verification.overall,
                lint: t.packet.verification.lint,
                types: t.packet.verification.types,
                security: t.packet.verification.security,
              })),
              sessionTokens: outcome.sessionTokens,
              sessionCost: outcome.sessionCost,
            },
            null,
            2,
          ),
        );
      } else {
        console.log(`\nCorrection Loop — ${outcome.turns.length} turns`);
        for (let i = 0; i < outcome.turns.length; i++) {
          const t = outcome.turns[i]!;
          console.log(
            `  Turn ${i + 1}: ${t.action} — ${t.rationale} [${t.workerTokens} tokens, ${t.packet.verification.overall}]`,
          );
        }
        console.log(`\nFinal action: ${outcome.finalAction}`);
        console.log(`Final verdict: ${outcome.finalVerdict} (${outcome.sourceOfTruth})`);
        if (outcome.sourceOfTruth === "task-validator") {
          console.log(
            `Task validator: ${outcome.taskValidation.status} — ${outcome.taskValidation.reason ?? outcome.taskValidation.validator}`,
          );
        }
        console.log(`Session tokens: ${outcome.sessionTokens}`);
        console.log(`Session cost: $${outcome.sessionCost.toFixed(4)}`);
      }
    });
}
