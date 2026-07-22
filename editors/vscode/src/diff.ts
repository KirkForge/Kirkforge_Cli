import * as vscode from 'vscode';
import { EditEvent } from './protocol';

let pendingEdit: { event: EditEvent; workspaceRoot: string; afterUri: vscode.Uri } | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;

export async function showEditDiff(
  event: EditEvent,
  workspaceRoot: string
): Promise<void> {
  const targetUri = vscode.Uri.file(joinPath(workspaceRoot, event.path));
  let before: string;
  try {
    const doc = await vscode.workspace.openTextDocument(targetUri);
    before = doc.getText();
  } catch {
    before = '';
  }
  const after = event.old_string
    ? before.replace(event.old_string, event.new_string ?? '')
    : event.new_string ?? before;
  const afterUri = await writeTempDocument(event.path, after);
  await vscode.commands.executeCommand(
    'vscode.diff',
    targetUri,
    afterUri,
    `KirkForge: ${event.path}`
  );
  pendingEdit = { event, workspaceRoot, afterUri };
  showStatusBar();
}

export function acceptEdit(): void {
  if (!pendingEdit) {
    return;
  }
  const { event, workspaceRoot } = pendingEdit;
  const targetPath = joinPath(workspaceRoot, event.path);
  const encoder = new TextEncoder();
  let content: string;
  try {
    content = require('fs').readFileSync(targetPath, 'utf-8');
  } catch {
    content = '';
  }
  const after = event.old_string
    ? content.replace(event.old_string, event.new_string ?? '')
    : event.new_string ?? content;
  void vscode.workspace.fs.writeFile(
    vscode.Uri.file(targetPath),
    encoder.encode(after)
  );
  clearPendingEdit();
  void vscode.window.showInformationMessage(`KirkForge: Applied edit to ${event.path}`);
}

export function rejectEdit(): void {
  if (!pendingEdit) {
    return;
  }
  clearPendingEdit();
  void vscode.window.showInformationMessage('KirkForge: Edit rejected');
}

export function getPendingEdit(): typeof pendingEdit {
  return pendingEdit;
}

function showStatusBar(): void {
  if (!statusBarItem) {
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
    statusBarItem.command = 'kirkforge.acceptEdit';
    statusBarItem.text = '$(edit) KirkForge: Edit pending →';
    statusBarItem.tooltip = 'Click to accept the pending edit';
  }
  statusBarItem.show();
}

function clearPendingEdit(): void {
  pendingEdit = undefined;
  statusBarItem?.hide();
}

async function writeTempDocument(relativePath: string, content: string): Promise<vscode.Uri> {
  const tmpDir = process.env.TMPDIR ?? process.env.TEMP ?? '/tmp';
  const uri = vscode.Uri.file(
    `${tmpDir}/kirkforge-diff-${Date.now()}-${relativePath.replace(/[/\\]/g, '_')}`
  );
  await vscode.workspace.fs.writeFile(uri, Buffer.from(content, 'utf-8'));
  return uri;
}

function joinPath(root: string, relative: string): string {
  return root.replace(/[/\\]$/, '') + '/' + relative.replace(/^[/\\]+/, '');
}