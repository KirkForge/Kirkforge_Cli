import * as vscode from 'vscode';
import { TodoUpdateEvent } from '../protocol';

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
    this.items = event.items.map((it, i) => new TodoItem(i, it.text, it.done));
    this._onDidChange.fire();
  }

  getChildren(element?: TodoItem): TodoItem[] {
    return element ? [] : this.items;
  }

  getTreeItem(element: TodoItem): vscode.TreeItem {
    return element;
  }
}

class TodoItem extends vscode.TreeItem {
  constructor(
    id: number,
    label: string,
    done: boolean
  ) {
    super(label, vscode.TreeItemCollapsibleState.None);
    this.contextValue = 'todoItem';
    this.checkboxState = done ? vscode.TreeItemCheckboxState.Checked : vscode.TreeItemCheckboxState.Unchecked;
    this.id = String(id);
  }
}
