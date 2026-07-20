import * as vscode from 'vscode';
import { EditEvent } from './protocol';

export async function showEditDiff(
  event: EditEvent,
  workspaceRoot: string
): Promise<void> {
  const targetUri = vscode.Uri.file(joinPath(workspaceRoot, event.path));
  const doc = await vscode.workspace.openTextDocument(targetUri);
  const before = doc.getText();
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
}

async function writeTempDocument(relativePath: string, content: string): Promise<vscode.Uri> {
  const tmpDir = process.env.TMPDIR ?? process.env.TEMP ?? '/tmp';
  const uri = vscode.Uri.file(`${tmpDir}/kirkforge-diff-${Date.now()}-${relativePath.replace(/[/\\]/g, '_')}`);
  await vscode.workspace.fs.writeFile(uri, Buffer.from(content, 'utf-8'));
  return uri;
}

function joinPath(root: string, relative: string): string {
  return root.replace(/[/\\]$/, '') + '/' + relative.replace(/^[/\\]+/, '');
}
