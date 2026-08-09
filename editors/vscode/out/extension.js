"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.activate = activate;
exports.activateWithApi = activateWithApi;
exports.deactivate = deactivate;
const chatPanel_1 = require("./panels/chatPanel");
const todoPanel_1 = require("./panels/todoPanel");
const bridge_1 = require("./bridge");
const diff_1 = require("./diff");
const lspBridge_1 = require("./lspBridge");
function activate(context) {
    const vscodeApi = require('vscode');
    activateWithApi(context, vscodeApi);
}
function activateWithApi(context, vscode) {
    const chatPanel = new chatPanel_1.ChatPanel(context);
    const todoPanel = new todoPanel_1.TodoPanel(context);
    let bridge;
    let lspBridge;
    const startPanel = vscode.commands.registerCommand('kirkforge.startPanel', async () => {
        const config = vscode.workspace.getConfiguration('kirkforge');
        const binaryPath = config.get('binaryPath', 'kirkforge');
        const folders = vscode.workspace.workspaceFolders;
        if (!folders || folders.length === 0) {
            void vscode.window.showWarningMessage('KirkForge needs an open workspace folder.');
            return;
        }
        const cwd = folders[0].uri.fsPath;
        bridge?.stop();
        bridge = new bridge_1.KirkForgeBridge({ binaryPath, cwd, outputFormat: 'ndjson' });
        chatPanel.setBridge(bridge);
        bridge.on('event', (event) => {
            chatPanel.handleEvent(event);
            if (event.type === 'todo_update') {
                todoPanel.handleUpdate(event);
            }
            if (event.type === 'edit') {
                void (0, diff_1.showEditDiff)(event, cwd);
            }
        });
        bridge.on('stderr', (line) => {
            chatPanel.handleEvent({
                type: 'tool_result',
                name: 'stderr',
                success: true,
                output: line,
            });
        });
        bridge.on('exit', (code) => {
            chatPanel.handleEvent({
                type: 'tool_result',
                name: 'kirkforge',
                success: code === 0,
                output: `kirkforge exited with code ${code ?? 'unknown'}`,
            });
        });
        bridge.on('error', (err) => {
            void vscode.window.showErrorMessage(`KirkForge error: ${err.message}`);
        });
        lspBridge = new lspBridge_1.LspBridge(cwd, (diags) => {
            if (bridge) {
                for (const entry of diags) {
                    bridge.writeLine(JSON.stringify({
                        type: 'diagnostics',
                        uri: entry.file,
                        diagnostics: entry.diagnostics,
                    }));
                }
            }
        });
        lspBridge.start();
        bridge.start();
        void vscode.window.showInformationMessage('KirkForge panel session started.');
    });
    const startTerminal = vscode.commands.registerCommand('kirkforge.startTerminal', () => {
        const config = vscode.workspace.getConfiguration('kirkforge');
        const binaryPath = config.get('binaryPath', 'kirkforge');
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
        (0, diff_1.acceptEdit)();
    });
    const rejectEditCmd = vscode.commands.registerCommand('kirkforge.rejectEdit', () => {
        (0, diff_1.rejectEdit)();
    });
    context.subscriptions.push(startPanel, startTerminal, acceptEditCmd, rejectEditCmd);
}
function deactivate() {
    // Bridge and LspBridge are disposed via context.subscriptions when the extension deactivates.
}
//# sourceMappingURL=extension.js.map