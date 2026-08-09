import * as vscode from 'vscode';
import { TodoUpdateEvent } from '../protocol';
import { formatTodoHtml } from '../format';

export class TodoPanel implements vscode.TreeDataProvider<TodoItem> {
  public static readonly viewType = 'kirkforge.todo';
  private items: TodoItem[] = [];
  private readonly _onDidChange = new vscode.EventEmitter<TodoItem | undefined | void>();
  readonly onDidChangeTreeData = this._onDidChange.event;

  constructor(context: vscode.ExtensionContext) {
    vscode.window.createTreeView(TodoPanel.viewType, { treeDataProvider: this });
    context.subscriptions.push(this._onDidChange);
  }

  handleUpdate(event: TodoUpdateEvent): void {
    this.items = event.items.map((it, i) => {
      const state = it.done ? 'completed' : it.in_progress ? 'in_progress' : 'pending';
      return new TodoItem(i, it.text, state);
    });
    this._onDidChange.fire();
  }

  getChildren(element?: TodoItem): TodoItem[] {
    return element ? [] : this.items;
  }

  getTreeItem(element: TodoItem): vscode.TreeItem {
    return element;
  }
}

export class TodoItem extends vscode.TreeItem {
  constructor(
    id: number,
    label: string,
    state: 'pending' | 'in_progress' | 'completed'
  ) {
    super(label, vscode.TreeItemCollapsibleState.None);
    this.contextValue = 'todoItem';
    this.id = String(id);
    if (state === 'completed') {
      this.iconPath = new vscode.ThemeIcon('check');
      this.description = 'done';
    } else if (state === 'in_progress') {
      this.iconPath = new vscode.ThemeIcon('sync~spin');
      this.description = 'in progress';
    } else {
      this.iconPath = new vscode.ThemeIcon('circle-outline');
      this.description = 'pending';
    }
  }
}

export { formatTodoHtml };