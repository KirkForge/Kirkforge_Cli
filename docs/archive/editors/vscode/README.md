# KirkForge for VS Code

Open-source extension that launches [KirkForge](https://github.com/KirkForge/KirkForge-Cli) inside the VS Code integrated terminal.

## Usage

1. Open a workspace folder.
2. Open the Command Palette (`Ctrl+Shift+P` / `Cmd+Shift+P`).
3. Run **KirkForge: Start KirkForge**.

The extension spawns `kirkforge run` with the workspace root as the working directory and lets the existing ratatui/crossterm TUI handle PTY rendering and resize events.

## Configuration

- `kirkforge.binaryPath` — path to the `kirkforge` binary (default: `kirkforge`).

## Packaging

```bash
npm run build
npm run package:vsce   # produces kirkforge-vscode-*.vsix
npm run package:ovsx  # produces kirkforge-vscode-*.vsix (Open VSX)
```
