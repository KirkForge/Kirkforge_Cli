# @kirkforge/mcp

KirkForge MCP Server — exposes verification, correction, and routing tools via Model Context Protocol (stdio transport).

Compatible with Claude Desktop, Codex CLI, Copilot, and any MCP host.

## Tools exposed

| Tool                                | Description                                   |
| ----------------------------------- | --------------------------------------------- |
| `kirkforge_verify_workspace`        | Run deterministic verification on a workspace |
| `kirkforge_doctor`                  | Check tool availability                       |
| `kirkforge_record_observation`      | Record task outcome for routing memory        |
| `kirkforge_recall_routing_bias`     | Recall routing recommendation                 |
| `kirkforge_build_correction_prompt` | Generate correction prompt from a packet      |

## Usage

```bash
npx @kirkforge/mcp
```

Or add to your MCP host configuration:

```json
{
  "mcpServers": {
    "kirkforge": {
      "command": "npx",
      "args": ["@kirkforge/mcp"]
    }
  }
}
```

## License

Apache-2.0
