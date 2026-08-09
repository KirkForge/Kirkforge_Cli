import type * as vscode from 'vscode';
import { ChatPanel } from './panels/chatPanel';
import { TodoPanel } from './panels/todoPanel';
import { KirkForgeBridge } from './bridge';
import { showEditDiff, acceptEdit, rejectEdit } from './diff';
import { LspBridge, DiagnosticEntry } from './lspBridge';
import { BridgeEvent } from './protocol';

export function activate(context: vscode.ExtensionContext): void {
  const vscodeApi = require('vscode') as typeof import('vscode');
  activateWithApi(context, vscodeApi);
}

export function activateWithApi(
  context: { subscriptions: { dispose: () => void }[] },
  vscode: typeof import('vscode')
): void {
  const chatPanel = new ChatPanel(context as vscode.ExtensionContext);
  const todoPanel = new TodoPanel(context as vscode.ExtensionContext);

  let bridge: KirkForgeBridge | undefined;
  let lspBridge: LspBridge | undefined;

  const startPanel = vscode.commands.registerCommand('kirkforge.startPanel', async () => {
    const config = vscode.workspace.getConfiguration('kirkforge');
    const binaryPath = config.get<string>('binaryPath', 'kirkforge');
    const folders = vscode.workspace.workspaceFolders;
    if (!folders || folders.length === 0) {
      void vscode.window.showWarningMessage('KirkForge needs an open workspace folder.');
      return;
    }
    const cwd = folders[0].uri.fsPath;

    bridge?.stop();
    bridge = new KirkForgeBridge({ binaryPath, cwd, outputFormat: 'ndjson' });
    chatPanel.setBridge(bridge);

    bridge.on('event', (event: BridgeEvent) => {
      chatPanel.handleEvent(event);
      if (event.type === 'todo_update') {
        todoPanel.handleUpdate(event);
      }
      if (event.type === 'edit') {
        void showEditDiff(event, cwd);
      }
    });
    bridge.on('stderr', (line: string) => {
      chatPanel.handleEvent({
        type: 'tool_result',
        name: 'stderr',
        success: true,
        output: line,
      });
    });
    bridge.on('exit', (code: number | null) => {
      chatPanel.handleEvent({
        type: 'tool_result',
        name: 'kirkforge',
        success: code === 0,
        output: `kirkforge exited with code ${code ?? 'unknown'}`,
      });
    });
    bridge.on('error', (err: Error) => {
      void vscode.window.showErrorMessage(`KirkForge error: ${err.message}`);
    });

    lspBridge = new LspBridge(cwd, (diags: DiagnosticEntry[]) => {
      if (bridge) {
        for (const entry of diags) {
          bridge.writeLine(
            JSON.stringify({
              type: 'diagnostics',
              uri: entry.file,
              diagnostics: entry.diagnostics,
            })
          );
        }
      }
    });
    lspBridge.start();

    bridge.start();
    void vscode.window.showInformationMessage('KirkForge panel session started.');
  });

  const startTerminal = vscode.commands.registerCommand('kirkforge.startTerminal', () => {
    const config = vscode.workspace.getConfiguration('kirkforge');
    const binaryPath = config.get<string>('binaryPath', 'kirkforge');
    const folders = vscode.workspace.workspaceFolders;
    if (!folders || folders.length === 0) {
      void vscode.window.showWarningMessage('KirkForge needs an open workspace folder.');
      return;
    }
    const terminal = vscode.window.createTerminal({
      name: 'KirkForge',
      cwd: folders[0].uri.fsPath,
      shellPath: binaryPath,
      shellArgs: ['run'],
    });
    terminal.show();
  });

  const acceptEditCmd = vscode.commands.registerCommand('kirkforge.acceptEdit', () => {
    acceptEdit();
  });

  const rejectEditCmd = vscode.commands.registerCommand('kirkforge.rejectEdit', () => {
    rejectEdit();
  });

  context.subscriptions.push(startPanel, startTerminal, acceptEditCmd, rejectEditCmd);
}

export function deactivate(): void {
  // Bridge and LspBridge are disposed via context.subscriptions when the extension deactivates.
}