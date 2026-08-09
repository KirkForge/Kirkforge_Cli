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
exports.LspBridge = void 0;
const vscode = __importStar(require("vscode"));
class LspBridge {
    workspaceRoot;
    onDiagnostics;
    debounceTimer;
    constructor(workspaceRoot, onDiagnostics) {
        this.workspaceRoot = workspaceRoot;
        this.onDiagnostics = onDiagnostics;
    }
    async query(req) {
        switch (req.query) {
            case 'symbol':
                if (!req.symbol) {
                    return { results: [] };
                }
                const symbols = await vscode.commands.executeCommand('vscode.executeWorkspaceSymbolProvider', req.symbol);
                return { results: symbols ?? [] };
            case 'type': {
                if (!req.file) {
                    return { results: [] };
                }
                const uri = vscode.Uri.file(this.joinPath(req.file));
                const definitions = (await vscode.commands.executeCommand('vscode.executeTypeDefinitionProvider', uri, new vscode.Position(0, 0))) ?? [];
                return { results: Array.isArray(definitions) ? definitions : [definitions] };
            }
            case 'diagnostics': {
                const all = vscode.languages.getDiagnostics();
                const target = req.file ? this.joinPath(req.file) : undefined;
                const filtered = target
                    ? all.filter(([uri]) => uri.fsPath === target)
                    : all;
                return {
                    results: filtered.map(([uri, diagnostics]) => ({
                        file: uri.fsPath,
                        diagnostics: diagnostics.map((d) => ({
                            message: d.message,
                            severity: d.severity,
                            range: d.range,
                        })),
                    })),
                };
            }
            default:
                return { results: [] };
        }
    }
    start() {
        vscode.workspace.onDidSaveTextDocument(() => this.collectAndSend());
        vscode.workspace.onDidChangeTextDocument(() => {
            if (this.debounceTimer) {
                clearTimeout(this.debounceTimer);
            }
            this.debounceTimer = setTimeout(() => this.collectAndSend(), 2000);
        });
    }
    collectAndSend() {
        const all = vscode.languages.getDiagnostics();
        const entries = all.map(([uri, diagnostics]) => ({
            file: uri.fsPath,
            diagnostics: diagnostics.map((d) => ({
                message: d.message,
                severity: d.severity,
                range: d.range,
            })),
        }));
        this.onDiagnostics(entries);
    }
    joinPath(relative) {
        return this.workspaceRoot.replace(/[/\\]$/, '') + '/' + relative.replace(/^[/\\]+/, '');
    }
}
exports.LspBridge = LspBridge;
//# sourceMappingURL=lspBridge.js.map