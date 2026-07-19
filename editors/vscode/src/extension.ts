import type * as vscode from 'vscode';

export function activate(context: vscode.ExtensionContext): void {
  const vscodeApi = require('vscode') as typeof import('vscode');
  activateWithApi(context, vscodeApi);
}

export function activateWithApi(
  context: { subscriptions: { dispose: () => void }[] },
  vscode: typeof import('vscode')
): void {
  const disposable = vscode.commands.registerCommand(
    'kirkforge.start',
    () => startKirkForge(vscode)
  );
  context.subscriptions.push(disposable);
}

export function deactivate(): void {
  // Nothing to clean up; VS Code owns the terminal lifecycle.
}

export async function startKirkForge(vscode: typeof import('vscode')): Promise<void> {
  const config = vscode.workspace.getConfiguration('kirkforge');
  const binaryPath = config.get<string>('binaryPath', 'kirkforge');

  const folders = vscode.workspace.workspaceFolders;
  if (!folders || folders.length === 0) {
    void vscode.window.showWarningMessage('KirkForge needs an open workspace folder.');
    return;
  }

  const cwd = folders[0].uri.fsPath;

  const terminal = vscode.window.createTerminal({
    name: 'KirkForge',
    cwd,
    shellPath: binaryPath,
    shellArgs: ['run'],
    // The integrated terminal already provides a PTY. ratatui/crossterm
    // handle PTY resize and rendering, so no custom Pseudoterminal is
    // required on the extension side.
  });

  terminal.show();
}
