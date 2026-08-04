#!/usr/bin/env node

import { Command } from "commander";
import { VERSION } from "./shared.js";
import { registerDelegate } from "./commands/delegate.js";
import { registerRun } from "./commands/run.js";
import { registerDecompose } from "./commands/decompose.js";
import { registerVerify } from "./commands/verify.js";
import { registerDoctor } from "./commands/doctor.js";
import { registerPrompt } from "./commands/prompt.js";
import { registerObserve } from "./commands/observe.js";
import { registerRecall } from "./commands/recall.js";
import { registerRecallDecomposition } from "./commands/recall-decomposition.js";
import { registerVerifyWorkspace } from "./commands/verify-workspace.js";
import { registerTools } from "./commands/tools.js";
import { registerHealth } from "./commands/health.js";
import { registerAuditVerify } from "./commands/audit-verify.js";
import { registerServe } from "./commands/serve.js";

const program = new Command();
program.name("kirkforge").description("Deterministic LLM output verification CLI").version(VERSION);

registerDelegate(program);
registerRun(program);
registerDecompose(program);
registerVerify(program);
registerDoctor(program);
registerPrompt(program);
registerObserve(program);
registerRecall(program);
registerRecallDecomposition(program);
registerVerifyWorkspace(program);
registerTools(program);
registerHealth(program);
registerAuditVerify(program);
registerServe(program);

program.parse();
