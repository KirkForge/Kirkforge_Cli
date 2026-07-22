# ADR-040: VS Code extension full surface

## Status

Accepted

## Context

ADR-019 designed the VS Code extension and ADR-026 designed the NDJSON bridge. The scaffold was 486L of TypeScript that built clean and had 4 passing tests, but it only had skeleton implementations of the diff viewer, chat panel, TODO panel, and LSP bridge. The extension could not be installed as a `.vsix`.

## Decision

Promote the VS Code extension to a shippable state with:

1. **Inline diffs** (`diff.ts`): `showEditDiff` opens a VS Code diff view comparing old vs new file content. `kirkforge.acceptEdit` writes the new content to the target file; `kirkforge.rejectEdit` dismisses. A status bar item shows "Edit pending" when there's an unaccepted edit.

2. **TODO panel** (`todoPanel.ts`): A tree data provider rendering `todo_update` events with three states: completed (green check), in_progress (yellow spinner), pending (gray circle). Pure HTML renderer (`formatTodoHtml`) extracted into `format.ts` for testability.

3. **Chat panel** (`chatPanel.ts`): A webview panel rendering messages from the agent with an input field and send button. User input calls `bridge.sendPrompt()`. Tool calls render as collapsed `<details>` blocks. Token streaming appends to the last assistant message.

4. **LSP bridge** (`lspBridge.ts`): Collects diagnostics from `vscode.languages.getDiagnostics()` on file save and on a 2-second debounce after edits, then sends them to the agent as `diagnostics` events via the bridge.

5. **Bridge improvements** (`bridge.ts`): Added `sendPrompt(text)` and `sendApproval(id, approved)` methods that write NDJSON lines to the kirkforge stdin. Error handling emits `error` events displayed as VS Code notifications.

6. **Pure utilities** (`format.ts`): Extracted `escapeHtml`, `truncate`, and `formatTodoHtml` into a module with no vscode dependency for testability.

7. **`.vsix` packaging**: `@vscode/vsce` as a dev dependency. `npm run package:vsce` produces `kirkforge-vscode-0.2.0.vsix`. `.vscodeignore` excludes source, test, and dev files.

8. **CI `vscode` job**: Builds, tests, and packages the extension on every PR. Uploads the `.vsix` as a build artifact.

## Consequences

- Positive: The agent is now usable inside VS Code with inline diffs, a chat interface, and LSP diagnostics piped back to the model. No more CLI-only friction.
- Positive: 13 tests pass (6 protocol, 5 format, 2 bridge NDJSON format). All are pure-function tests with no vscode dependency.
- Negative: TypeScript dependency in a Rust project. CI time +1-2 min for the vscode job.
- Negative: `.vsix` maintenance burden — version bumps, dependency updates, compatibility testing.