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
exports.formatTodoHtml = exports.TodoItem = exports.TodoPanel = void 0;
const vscode = __importStar(require("vscode"));
const format_1 = require("../format");
Object.defineProperty(exports, "formatTodoHtml", { enumerable: true, get: function () { return format_1.formatTodoHtml; } });
class TodoPanel {
    static viewType = 'kirkforge.todo';
    items = [];
    _onDidChange = new vscode.EventEmitter();
    onDidChangeTreeData = this._onDidChange.event;
    constructor(context) {
        vscode.window.createTreeView(TodoPanel.viewType, { treeDataProvider: this });
        context.subscriptions.push(this._onDidChange);
    }
    handleUpdate(event) {
        this.items = event.items.map((it, i) => {
            const state = it.done ? 'completed' : it.in_progress ? 'in_progress' : 'pending';
            return new TodoItem(i, it.text, state);
        });
        this._onDidChange.fire();
    }
    getChildren(element) {
        return element ? [] : this.items;
    }
    getTreeItem(element) {
        return element;
    }
}
exports.TodoPanel = TodoPanel;
class TodoItem extends vscode.TreeItem {
    constructor(id, label, state) {
        super(label, vscode.TreeItemCollapsibleState.None);
        this.contextValue = 'todoItem';
        this.id = String(id);
        if (state === 'completed') {
            this.iconPath = new vscode.ThemeIcon('check');
            this.description = 'done';
        }
        else if (state === 'in_progress') {
            this.iconPath = new vscode.ThemeIcon('sync~spin');
            this.description = 'in progress';
        }
        else {
            this.iconPath = new vscode.ThemeIcon('circle-outline');
            this.description = 'pending';
        }
    }
}
exports.TodoItem = TodoItem;
//# sourceMappingURL=todoPanel.js.map