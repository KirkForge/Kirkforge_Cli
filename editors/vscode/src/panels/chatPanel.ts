import * as vscode from 'vscode';
import { BridgeEvent } from '../protocol';

export class ChatPanel {
  public static readonly viewType = 'kirkforge.chat';
  private readonly panel: vscode.WebviewPanel;
  private messages: { role: string; content: string }[] = [];

  constructor(context: vscode.ExtensionContext) {
    this.panel = vscode.window.createWebviewPanel(
      ChatPanel.viewType,
      'KirkForge Chat',
      vscode.ViewColumn.One,
      { enableScripts: true, retainContextWhenHidden: true }
    );
    this.panel.webview.html = this.render();
    context.subscriptions.push(this.panel);
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
          content: `\ud83d\udd27 ${event.name}(${JSON.stringify(event.arguments)})`,
        });
        break;
      case 'tool_result':
        this.messages.push({
          role: 'tool',
          content: event.success ? `\u2705 ${event.name}` : `\u274c ${event.name}: ${event.error ?? ''}`,
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
        const cls = m.role === 'user' ? 'user' : m.role === 'assistant' ? 'assistant' : 'tool';
        return `<div class="msg ${cls}">${this.escapeHtml(m.content)}</div>`;
      })
      .join('\n');
    return `<!DOCTYPE html>
<html>
<head>
  <style>
    body { font-family: system-ui, sans-serif; padding: 12px; }
    .msg { margin: 8px 0; padding: 8px; border-radius: 6px; white-space: pre-wrap; }
    .user { background: #0066cc; color: white; }
    .assistant { background: #2d2d2d; color: #f0f0f0; }
    .tool { background: #f4f4f4; color: #333; font-family: monospace; }
  </style>
</head>
<body>${rows}</body>
</html>`;
  }

  private escapeHtml(text: string): string {
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
  }
}
