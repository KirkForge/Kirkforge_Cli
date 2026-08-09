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
exports.ChatPanel = void 0;
const vscode = __importStar(require("vscode"));
const format_1 = require("../format");
class ChatPanel {
    static viewType = 'kirkforge.chat';
    panel;
    messages = [];
    bridge;
    constructor(context) {
        this.panel = vscode.window.createWebviewPanel(ChatPanel.viewType, 'KirkForge Chat', vscode.ViewColumn.One, { enableScripts: true, retainContextWhenHidden: true });
        this.panel.webview.html = this.render();
        this.panel.webview.onDidReceiveMessage((msg) => {
            if (msg.type === 'sendPrompt' && msg.text && this.bridge) {
                this.bridge.sendPrompt(msg.text);
            }
        }, undefined, context.subscriptions);
        context.subscriptions.push(this.panel);
    }
    setBridge(bridge) {
        this.bridge = bridge;
    }
    handleEvent(event) {
        switch (event.type) {
            case 'message':
                this.messages.push({ role: event.role, content: event.content });
                break;
            case 'token':
                if (this.messages.length > 0) {
                    const last = this.messages[this.messages.length - 1];
                    if (last.role === 'assistant') {
                        last.content += event.content;
                    }
                    else {
                        this.messages.push({ role: 'assistant', content: event.content });
                    }
                }
                else {
                    this.messages.push({ role: 'assistant', content: event.content });
                }
                break;
            case 'tool_call':
                this.messages.push({
                    role: 'tool',
                    content: `\uD83D\uDD27 ${event.name}(${(0, format_1.truncate)(JSON.stringify(event.arguments), 120)})`,
                    collapsed: true,
                });
                break;
            case 'tool_result':
                this.messages.push({
                    role: 'tool',
                    content: event.success
                        ? `\u2705 ${event.name}`
                        : `\u274C ${event.name}: ${event.error ?? ''}`,
                });
                break;
            default:
                return;
        }
        this.panel.webview.html = this.render();
    }
    render() {
        const rows = this.messages
            .map((m) => {
            const cls = m.role === 'user'
                ? 'user'
                : m.role === 'assistant'
                    ? 'assistant'
                    : 'tool';
            const toggle = m.collapsed
                ? `<details><summary>${(0, format_1.escapeHtml)(m.content)}</summary></details>`
                : (0, format_1.escapeHtml)(m.content);
            return `<div class="msg ${cls}">${toggle}</div>`;
        })
            .join('\n');
        return `<!DOCTYPE html>
<html>
<head>
  <style>
    body { font-family: system-ui, sans-serif; padding: 12px; margin: 0; display: flex; flex-direction: column; height: 100vh; }
    #messages { flex: 1; overflow-y: auto; }
    .msg { margin: 8px 0; padding: 8px; border-radius: 6px; white-space: pre-wrap; }
    .user { background: #0066cc; color: white; }
    .assistant { background: #2d2d2d; color: #f0f0f0; }
    .tool { background: #f4f4f4; color: #333; font-family: monospace; font-size: 0.85em; }
    #input-area { display: flex; padding: 8px 0; }
    #prompt-input { flex: 1; padding: 6px; font-size: 14px; border: 1px solid #ccc; border-radius: 4px; }
    #send-btn { margin-left: 8px; padding: 6px 16px; background: #0066cc; color: white; border: none; border-radius: 4px; cursor: pointer; }
  </style>
</head>
<body>
  <div id="messages">${rows}</div>
  <div id="input-area">
    <input type="text" id="prompt-input" placeholder="Type a message..." />
    <button id="send-btn">Send</button>
  </div>
  <script>
    const vscode = acquireVsCodeApi();
    document.getElementById('send-btn').addEventListener('click', () => {
      const input = document.getElementById('prompt-input');
      const text = input.value.trim();
      if (text) {
        vscode.postMessage({ type: 'sendPrompt', text });
        input.value = '';
      }
    });
    document.getElementById('prompt-input').addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        document.getElementById('send-btn').click();
      }
    });
  </script>
</body>
</html>`;
    }
}
exports.ChatPanel = ChatPanel;
//# sourceMappingURL=chatPanel.js.map