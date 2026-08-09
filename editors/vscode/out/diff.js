"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.showEditDiff = showEditDiff;
exports.acceptEdit = acceptEdit;
exports.rejectEdit = rejectEdit;
exports.getPendingEdit = getPendingEdit;
const vscode = __importStar(require("vscode"));
let pendingEdit;
let statusBarItem;
async function showEditDiff(event, workspaceRoot) {
    const targetUri = vscode.Uri.file(joinPath(workspaceRoot, event.path));
    let before;
    try {
        const doc = await vscode.workspace.openTextDocument(targetUri);
        before = doc.getText();
    }
    catch {
        before = '';
    }
    const after = event.old_string
        ? before.replace(event.old_string, event.new_string ?? '')
        : event.new_string ?? before;
    const afterUri = await writeTempDocument(event.path, after);
    await vscode.commands.executeCommand('vscode.diff', targetUri, afterUri, `KirkForge: ${event.path}`);
    pendingEdit = { event, workspaceRoot, afterUri };
    showStatusBar();
}
function acceptEdit() {
    if (!pendingEdit) {
        return;
    }
    const { event, workspaceRoot } = pendingEdit;
    const targetPath = joinPath(workspaceRoot, event.path);
    const encoder = new TextEncoder();
    let content;
    try {
        content = require('fs').readFileSync(targetPath, 'utf-8');
    }
    catch {
        content = '';
    }
    const after = event.old_string
        ? content.replace(event.old_string, event.new_string ?? '')
        : event.new_string ?? content;
    void vscode.workspace.fs.writeFile(vscode.Uri.file(targetPath), encoder.encode(after));
    clearPendingEdit();
    void vscode.window.showInformationMessage(`KirkForge: Applied edit to ${event.path}`);
}
function rejectEdit() {
    if (!pendingEdit) {
        return;
    }
    clearPendingEdit();
    void vscode.window.showInformationMessage('KirkForge: Edit rejected');
}
function getPendingEdit() {
    return pendingEdit;
}
function showStatusBar() {
    if (!statusBarItem) {
        statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
        statusBarItem.command = 'kirkforge.acceptEdit';
        statusBarItem.text = '$(edit) KirkForge: Edit pending →';
        statusBarItem.tooltip = 'Click to accept the pending edit';
    }
    statusBarItem.show();
}
function clearPendingEdit() {
    pendingEdit = undefined;
    statusBarItem?.hide();
}
async function writeTempDocument(relativePath, content) {
    const tmpDir = process.env.TMPDIR ?? process.env.TEMP ?? '/tmp';
    const uri = vscode.Uri.file(`${tmpDir}/kirkforge-diff-${Date.now()}-${relativePath.replace(/[/\\]/g, '_')}`);
    await vscode.workspace.fs.writeFile(uri, Buffer.from(content, 'utf-8'));
    return uri;
}
function joinPath(root, relative) {
    return root.replace(/[/\\]$/, '') + '/' + relative.replace(/^[/\\]+/, '');
}
//# sourceMappingURL=diff.js.map