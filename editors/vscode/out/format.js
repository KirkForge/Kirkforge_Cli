"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.formatTodoHtml = formatTodoHtml;
exports.escapeHtml = escapeHtml;
exports.truncate = truncate;
function formatTodoHtml(items) {
    return items
        .map((it) => {
        const state = it.done ? 'completed' : it.in_progress ? 'in_progress' : 'pending';
        const color = state === 'completed' ? 'green' : state === 'in_progress' ? '#c8a800' : 'gray';
        const checkbox = it.done ? '\u2611' : it.in_progress ? '\u25A0' : '\u2610';
        return `<div style="color:${color}">${checkbox} ${escapeHtml(it.text)}</div>`;
    })
        .join('\n');
}
function escapeHtml(text) {
    return text
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;');
}
function truncate(s, max) {
    return s.length > max ? s.slice(0, max) + '...' : s;
}
//# sourceMappingURL=format.js.map