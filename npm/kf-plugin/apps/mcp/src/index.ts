#!/usr/bin/env node
/**
 * KirkForge MCP Server — exposes verification, correction, and routing tools
 * via Model Context Protocol (stdio transport).
 *
 * Compatible with Claude Desktop, Codex CLI, Copilot, and any MCP host.
 *
 * Usage:
 *   npx @kirkforge/mcp
 *   node apps/mcp/dist/index.js
 *
 * Tools exposed:
 *   - kirkforge_verify_workspace: Run deterministic verification
 *   - kirkforge_doctor: Check tool availability
 *   - kirkforge_record_observation: Record task outcome for routing memory
 *   - kirkforge_recall_routing_bias: Recall routing recommendation
 *   - kirkforge_build_correction_prompt: Generate correction prompt from a packet
 */

import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { CallToolRequestSchema, ListToolsRequestSchema } from "@modelcontextprotocol/sdk/types.js";
import {
  verifyWorkspace,
  doctor,
  buildCorrectionPrompt,
  recordObservation,
  recallRoutingBias,
} from "@kirkforge/plugin";
import { MemoryStore, InMemoryAdapter } from "@kirkforge/memory-palace";
import type { ReducedStatePacket } from "@kirkforge/correction-core";
import { z } from "zod";

// ── Runtime input validators ────────────────────────────────────────────────

function validateArgs<T>(
  args: unknown,
  schema: z.ZodType<T>,
  toolName: string,
): T | { error: string } {
  const result = schema.safeParse(args);
  if (!result.success) {
    return {
      error: `Invalid arguments for ${toolName}: ${result.error.issues.map((i) => `${i.path.join(".")}: ${i.message}`).join("; ")}`,
    };
  }
  return result.data;
}

const VerifyWorkspaceSchema = z.object({
  workspace: z.string().min(1),
  files: z.array(z.string()).optional(),
  language: z
    .enum(["typescript", "javascript", "python", "shell", "cpp", "c", "rust", "go", "sql", "text"])
    .optional(),
  description: z.string().optional(),
  taskId: z.string().optional(),
});

const RecordObservationSchema = z.object({
  taskId: z.string().min(1),
  description: z.string().min(1),
  language: z.string().min(1),
  mode: z.string().min(1),
  model: z.string().min(1),
  outcome: z.enum(["pass", "fail", "escalate", "error"]),
  durationMs: z.number().int().positive(),
  tokens: z.number().int().nonnegative().optional(),
  verifierOverall: z.string().optional(),
});

const RecallRoutingBiasSchema = z.object({
  taskDescription: z.string().min(1),
  workerModel: z.string().optional(),
});

const BuildCorrectionPromptSchema = z.object({
  packet: z.record(z.string(), z.unknown()),
  language: z.string().optional(),
  maxTokens: z.number().int().positive().optional(),
});

// ── Shared MemoryStore ─────────────────────────────────────────────────────

const memoryStore = new MemoryStore(new InMemoryAdapter());

// ── Tool definitions ───────────────────────────────────────────────────────

const TOOLS = [
  {
    name: "kirkforge_verify_workspace",
    description:
      "Run deterministic verification on a workspace. Returns a ReducedStatePacket with lint, type, security, change, and graph analysis results.",
    inputSchema: {
      type: "object",
      properties: {
        workspace: { type: "string", description: "Absolute path to the workspace directory" },
        files: {
          type: "array",
          items: { type: "string" },
          description: "Specific files to verify (optional)",
        },
        language: {
          type: "string",
          enum: [
            "typescript",
            "javascript",
            "python",
            "shell",
            "cpp",
            "c",
            "rust",
            "go",
            "sql",
            "text",
          ],
          description: "Task language",
        },
        description: {
          type: "string",
          description: "Task description for language profile detection",
        },
        taskId: { type: "string", description: "Task identifier (optional, auto-generated)" },
      },
      required: ["workspace"],
    },
  },
  {
    name: "kirkforge_doctor",
    description:
      "Check availability of all verification tools (eslint, tsc, ruff, pyright, bandit, git) and return a capability report.",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "kirkforge_record_observation",
    description: "Record a task outcome into the routing memory for future recall.",
    inputSchema: {
      type: "object",
      properties: {
        taskId: { type: "string", description: "Unique task identifier" },
        description: { type: "string", description: "Task description" },
        language: { type: "string", description: "Programming language" },
        mode: { type: "string", description: "Delegation mode used" },
        model: { type: "string", description: "Model used for the task" },
        outcome: {
          type: "string",
          enum: ["pass", "fail", "escalate", "error"],
          description: "Task outcome",
        },
        durationMs: { type: "number", description: "Task duration in milliseconds" },
        tokens: { type: "number", description: "Total tokens consumed (optional)" },
        verifierOverall: { type: "string", description: "Overall verifier result (optional)" },
      },
      required: ["taskId", "description", "language", "mode", "model", "outcome", "durationMs"],
    },
  },
  {
    name: "kirkforge_recall_routing_bias",
    description: "Recall routing recommendation for a task description based on past observations.",
    inputSchema: {
      type: "object",
      properties: {
        taskDescription: { type: "string", description: "Task description to match" },
        workerModel: { type: "string", description: "Current worker model (optional)" },
      },
      required: ["taskDescription"],
    },
  },
  {
    name: "kirkforge_build_correction_prompt",
    description:
      "Generate a correction prompt from a ReducedStatePacket for the worker model to fix issues.",
    inputSchema: {
      type: "object",
      properties: {
        packet: { type: "object", description: "ReducedStatePacket from verifyWorkspace" },
        language: { type: "string", description: "Task language for tool name mapping" },
        maxTokens: {
          type: "number",
          description: "Maximum tokens for the correction prompt (optional)",
        },
      },
      required: ["packet"],
    },
  },
];

// ── Server ──────────────────────────────────────────────────────────────────

const server = new Server(
  { name: "kirkforge-mcp", version: "1.0.0" },
  { capabilities: { tools: {} } },
);

server.setRequestHandler(ListToolsRequestSchema, async () => ({ tools: TOOLS }));

server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;

  try {
    switch (name) {
      case "kirkforge_verify_workspace": {
        const parsed = validateArgs(args, VerifyWorkspaceSchema, name);
        if ("error" in parsed)
          return { content: [{ type: "text", text: JSON.stringify(parsed) }], isError: true };
        const result = await verifyWorkspace(parsed);
        if (!result.ok) {
          return {
            content: [{ type: "text", text: JSON.stringify({ error: result.error.message }) }],
            isError: true,
          };
        }
        return { content: [{ type: "text", text: JSON.stringify(result.value, null, 2) }] };
      }

      case "kirkforge_doctor": {
        const report = await doctor();
        return { content: [{ type: "text", text: JSON.stringify(report, null, 2) }] };
      }

      case "kirkforge_record_observation": {
        const parsed = validateArgs(args, RecordObservationSchema, name);
        if ("error" in parsed)
          return { content: [{ type: "text", text: JSON.stringify(parsed) }], isError: true };
        const result = await recordObservation(parsed as any, memoryStore);
        if (!result.ok) {
          return {
            content: [{ type: "text", text: JSON.stringify({ error: result.error.message }) }],
            isError: true,
          };
        }
        return { content: [{ type: "text", text: "Observation recorded successfully" }] };
      }

      case "kirkforge_recall_routing_bias": {
        const parsed = validateArgs(args, RecallRoutingBiasSchema, name);
        if ("error" in parsed)
          return { content: [{ type: "text", text: JSON.stringify(parsed) }], isError: true };
        const result = await recallRoutingBias(
          parsed.taskDescription,
          parsed.workerModel,
          memoryStore,
        );
        if (!result.ok) {
          return {
            content: [{ type: "text", text: JSON.stringify({ error: result.error.message }) }],
            isError: true,
          };
        }
        return { content: [{ type: "text", text: JSON.stringify(result.value, null, 2) }] };
      }

      case "kirkforge_build_correction_prompt": {
        const parsed = validateArgs(args, BuildCorrectionPromptSchema, name);
        if ("error" in parsed)
          return { content: [{ type: "text", text: JSON.stringify(parsed) }], isError: true };
        const prompt = buildCorrectionPrompt(parsed.packet as unknown as ReducedStatePacket, {
          language: parsed.language,
          maxTokens: parsed.maxTokens,
        });
        return { content: [{ type: "text", text: prompt }] };
      }

      default:
        return { content: [{ type: "text", text: `Unknown tool: ${name}` }], isError: true };
    }
  } catch (e) {
    return {
      content: [
        { type: "text", text: `Tool error: ${e instanceof Error ? e.message : String(e)}` },
      ],
      isError: true,
    };
  }
});

// ── Start ───────────────────────────────────────────────────────────────────

async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  // Log to stderr so it doesn't interfere with MCP stdio protocol
  process.stderr.write("KirkForge MCP server running on stdio\n");
}

main().catch((e) => {
  process.stderr.write(`MCP server fatal: ${e.message}\n`);
  process.exit(1);
});
