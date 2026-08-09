import * as vscode from 'vscode';
import { BridgeEvent } from '../protocol';
import { KirkForgeBridge } from '../bridge';
import { escapeHtml, truncate } from '../format';

export class ChatPanel {
  public static readonly viewType = 'kirkforge.chat';
  private readonly panel: vscode.WebviewPanel;
  private messages: { role: string; content: string; collapsed?: boolean }[] = [];
  private bridge: KirkForgeBridge | undefined;

  constructor(context: vscode.ExtensionContext) {
    this.panel = vscode.window.createWebviewPanel(
      ChatPanel.viewType,
      'KirkForge Chat',
      vscode.ViewColumn.One,
      { enableScripts: true, retainContextWhenHidden: true }
    );
    this.panel.webview.html = this.render();
    this.panel.webview.onDidReceiveMessage(
      (msg: { type: string; text?: string }) => {
        if (msg.type === 'sendPrompt' && msg.text && this.bridge) {
          this.bridge.sendPrompt(msg.text);
        }
      },
      undefined,
      context.subscriptions
    );
    context.subscriptions.push(this.panel);
  }

  setBridge(bridge: KirkForgeBridge): void {
    this.bridge = bridge;
  }

  handleEvent(event: BridgeEvent): void {
    switch (event.type) {
      case 'message':
        this.messages.push({ role: event.role, content: event.content });
        break;
      case 'token':
        if (this.messages.length > 0) {
          const last = this.messages[this.messages.length - 1];
          if (last.role === 'assistant') {
            last.content += event.content;
          } else {
            this.messages.push({ role: 'assistant', content: event.content });
          }
        } else {
          this.messages.push({ role: 'assistant', content: event.content });
        }
        break;
      case 'tool_call':
        this.messages.push({
          role: 'tool',
          content: `\uD83D\uDD27 ${event.name}(${truncate(JSON.stringify(event.arguments), 120)})`,
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

  private render(): string {
    const rows = this.messages
      .map((m) => {
        const cls =
          m.role === 'user'
            ? 'user'
            : m.role === 'assistant'
              ? 'assistant'
              : 'tool';
        const toggle = m.collapsed
          ? `<details><summary>${escapeHtml(m.content)}</summary></details>`
          : escapeHtml(m.content);
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