import type { Command } from "commander";
import { createBootstrap } from "../bootstrap.js";

export function registerHealth(program: Command): void {
  program
    .command("health")
    .description("Show orchestrator health and SLO status")
    .action(async () => {
      const {
        orchestrator,
        shutdown: _shutdown,
        policyEngine: _policyEngine,
        auditLogger: _auditLogger,
      } = await createBootstrap({});
      const h = orchestrator.healthCheck();
      console.log(`Status:         ${h.status}`);
      console.log(
        `EventBus:       ${h.eventBus.running ? "running" : "stopped"} (inflight: ${h.eventBus.inflight})`,
      );
      console.log(`Memory:         ${h.memory}`);
      console.log(`Providers:      ${h.providers}`);
      console.log(`Delegations:    ${h.stats.totalDelegations}`);
      console.log(`Total tokens:   ${h.stats.totalTokens}`);

      const slo = await orchestrator.slo();
      if (slo) {
        console.log(`\n--- SLO Burn-Rate Report ---`);
        for (const w of slo.windows) {
          const pct = (w.rate * 100).toFixed(1);
          const budgetPct = (w.budgetRemaining * 100).toFixed(1);
          console.log(
            `  ${w.name}: rate=${pct}% budget=${budgetPct}% burn=${w.burnRate.toFixed(2)}x status=${w.status}`,
          );
        }
      } else {
        console.log(`\nSLO:           no prior runs — run tasks to populate SLO windows`);
      }
    });
}
