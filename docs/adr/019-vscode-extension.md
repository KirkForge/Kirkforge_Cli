# ADR 019: VS Code extension — Option A PTY wrapper MVP

## Status

Accepted (2026-07-19)

## Context

CLI-WORKORDER.md WO-1 asks for a KirkForge editor integration. Two options were on the table:

- **Option A:** a thin VS Code extension that launches `kirkforge run` in the integrated terminal. This reuses the existing ratatui/crossterm TUI with no protocol changes.
- **Option B:** an NDJSON bridge that talks to a headless KirkForge runtime and renders chat in a webview. This is more powerful but needs a new API surface, serialization, and UI.

## Decision

Choose **Option A** for the MVP.

### Why Option A

- Fastest path (~1 week), proving demand before committing to protocol work.
- Reuses the existing TUI, approval flow, undo stack, LSP client, and plugin system.
- No new runtime mode or IPC contract is required.
- Target is Open VSX (`ovsx publish`) only; no Microsoft Marketplace dependency. All dependencies are open-source.

### Why not Option B now

- It would need a stable NDJSON command/response protocol, cancellation, progress reporting, and file-attachment framing.
- A webview front-end duplicates the TUI and must reimplement approvals, streaming, and theming.
- Higher risk and longer timeline; defer until Option A demonstrates real usage.

## Architecture

1. **Vendored location:** `editors/vscode/` in the KirkForge-Cli repo, matching the `crates/*` and `npm/kirkforge-plugin/` vendored pattern.
2. **Extension manifest (`package.json`):**
   - Engine: `vscode ^1.85`.
   - Command: `kirkforge.start` ("KirkForge: Start KirkForge").
   - Setting: `kirkforge.binaryPath`, default `"kirkforge"`.
3. **Runtime seam (`src/extension.ts`):** `vscode.window.createTerminal({ name: 'KirkForge', cwd: workspaceRoot, shellPath: binaryPath, shellArgs: ['run'] })`. The integrated terminal already provides a PTY, so ratatui/crossterm get the same environment they expect in an external terminal.
4. **PTY resize:** crossterm already receives terminal resize events (`Event::Resize`) in the Rust TUI loop, which marks the app dirty and lets the next `terminal.draw()` use the current size. No extension-side resize handling is needed.

## Consequences

- **+1 editor package** under `editors/vscode/` with TypeScript, `npm run build`, and `npm test`.
- **+1 installable artifact** built with `vsce package` and `ovsx package`.
- **No Rust changes are required** for the wrapper, but the Rust gates are still run to ensure the TUI resize path stays green.
- **Upgrade path:** if demand materializes, the extension can keep Option A as the zero-config default and add Option B behind a setting or separate command. The PTY wrapper remains the fallback.

## Alternatives considered

- **(a) Option B NDJSON bridge now.** Rejected for the MVP; retained as the future upgrade path.
- **(b) Publish to the Microsoft Marketplace.** Rejected per the hard rule to target Open VSX and avoid closed-source/marketplace lock-in.
- **(c) Custom `Pseudoterminal` implementation in the extension.** Rejected; the built-in integrated terminal already provides a PTY, so a custom implementation would duplicate work without benefit.

## Test strategy

- TypeScript build gate (`npm run build`) and unit tests (`npm test`) run in `editors/vscode/`.
- `vsce package` and `ovsx package` produce installable `.vsix` artifacts.
- Rust matrix remains unchanged; the existing crossterm resize handler is verified rather than modified.
