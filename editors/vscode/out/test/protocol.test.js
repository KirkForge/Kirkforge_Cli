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
const node_test_1 = require("node:test");
const assert = __importStar(require("assert"));
const protocol_1 = require("../protocol");
const format_1 = require("../format");
(0, node_test_1.describe)('protocol', () => {
    (0, node_test_1.it)('parses turn_start', () => {
        const line = JSON.stringify({ type: 'turn_start', id: 't1', timestamp: '2026-07-20T00:00:00Z' });
        const ev = (0, protocol_1.parseEvent)(line);
        assert.strictEqual(ev?.type, 'turn_start');
        assert.strictEqual(ev.id, 't1');
    });
    (0, node_test_1.it)('parses token', () => {
        const line = JSON.stringify({ type: 'token', content: 'hello' });
        const ev = (0, protocol_1.parseEvent)(line);
        assert.strictEqual(ev?.type, 'token');
        assert.strictEqual(ev.content, 'hello');
    });
    (0, node_test_1.it)('parses diagnostics event', () => {
        const line = JSON.stringify({
            type: 'diagnostics',
            uri: '/tmp/test.rs',
            diagnostics: [{ message: 'unused variable', severity: 2, range: {} }],
        });
        const ev = (0, protocol_1.parseEvent)(line);
        assert.strictEqual(ev?.type, 'diagnostics');
        assert.strictEqual(ev.uri, '/tmp/test.rs');
    });
    (0, node_test_1.it)('parses todo_update with in_progress', () => {
        const line = JSON.stringify({
            type: 'todo_update',
            items: [
                { text: 'done task', done: true },
                { text: 'active task', done: false, in_progress: true },
                { text: 'pending task', done: false },
            ],
        });
        const ev = (0, protocol_1.parseEvent)(line);
        assert.strictEqual(ev?.type, 'todo_update');
        const items = ev.items;
        assert.strictEqual(items.length, 3);
        assert.strictEqual(items[0].done, true);
        assert.strictEqual(items[1].in_progress, true);
    });
    (0, node_test_1.it)('ignores unknown types', () => {
        const line = JSON.stringify({ type: 'future_event', payload: 1 });
        assert.strictEqual((0, protocol_1.parseEvent)(line), undefined);
    });
    (0, node_test_1.it)('ignores invalid json', () => {
        assert.strictEqual((0, protocol_1.parseEvent)('not json'), undefined);
    });
});
(0, node_test_1.describe)('format', () => {
    (0, node_test_1.it)('formatTodoHtml renders items with correct colors', () => {
        const html = (0, format_1.formatTodoHtml)([
            { text: 'done task', done: true },
            { text: 'active task', done: false, in_progress: true },
            { text: 'pending task', done: false },
        ]);
        assert.ok(html.includes('green'), 'completed item should be green');
        assert.ok(html.includes('#c8a800'), 'in_progress item should be yellow');
        assert.ok(html.includes('gray'), 'pending item should be gray');
        assert.ok(html.includes('\u2611'), 'completed item should have checked checkbox');
        assert.ok(html.includes('\u25A0'), 'in_progress item should have filled square');
        assert.ok(html.includes('\u2610'), 'pending item should have empty checkbox');
    });
    (0, node_test_1.it)('formatTodoHtml escapes HTML', () => {
        const html = (0, format_1.formatTodoHtml)([{ text: '<script>alert(1)</script>', done: false }]);
        assert.ok(!html.includes('<script>'), 'should escape HTML tags');
        assert.ok(html.includes('&lt;script&gt;'), 'should have escaped tags');
    });
    (0, node_test_1.it)('escapeHtml handles ampersands and angle brackets', () => {
        assert.strictEqual((0, format_1.escapeHtml)('a < b & c > d'), 'a &lt; b &amp; c &gt; d');
    });
    (0, node_test_1.it)('truncate short strings unchanged', () => {
        assert.strictEqual((0, format_1.truncate)('hello', 10), 'hello');
    });
    (0, node_test_1.it)('truncate long strings with ellipsis', () => {
        assert.strictEqual((0, format_1.truncate)('abcdefghij', 5), 'abcde...');
    });
});
(0, node_test_1.describe)('bridge NDJSON format', () => {
    (0, node_test_1.it)('sendPrompt format is valid JSON', () => {
        const line = JSON.stringify({ type: 'prompt', text: 'hello world' });
        const parsed = JSON.parse(line);
        assert.strictEqual(parsed.type, 'prompt');
        assert.strictEqual(parsed.text, 'hello world');
    });
    (0, node_test_1.it)('sendApproval format is valid JSON', () => {
        const line = JSON.stringify({ type: 'approval', id: 'call_123', approved: true });
        const parsed = JSON.parse(line);
        assert.strictEqual(parsed.type, 'approval');
        assert.strictEqual(parsed.id, 'call_123');
        assert.strictEqual(parsed.approved, true);
    });
});
//# sourceMappingURL=protocol.test.js.map