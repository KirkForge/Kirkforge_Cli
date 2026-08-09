import { describe, it } from 'node:test';
import * as assert from 'assert';
import { parseEvent } from '../protocol';
import { formatTodoHtml, escapeHtml, truncate } from '../format';

describe('protocol', () => {
  it('parses turn_start', () => {
    const line = JSON.stringify({ type: 'turn_start', id: 't1', timestamp: '2026-07-20T00:00:00Z' });
    const ev = parseEvent(line);
    assert.strictEqual(ev?.type, 'turn_start');
    assert.strictEqual((ev as any).id, 't1');
  });

  it('parses token', () => {
    const line = JSON.stringify({ type: 'token', content: 'hello' });
    const ev = parseEvent(line);
    assert.strictEqual(ev?.type, 'token');
    assert.strictEqual((ev as any).content, 'hello');
  });

  it('parses diagnostics event', () => {
    const line = JSON.stringify({
      type: 'diagnostics',
      uri: '/tmp/test.rs',
      diagnostics: [{ message: 'unused variable', severity: 2, range: {} }],
    });
    const ev = parseEvent(line);
    assert.strictEqual(ev?.type, 'diagnostics');
    assert.strictEqual((ev as any).uri, '/tmp/test.rs');
  });

  it('parses todo_update with in_progress', () => {
    const line = JSON.stringify({
      type: 'todo_update',
      items: [
        { text: 'done task', done: true },
        { text: 'active task', done: false, in_progress: true },
        { text: 'pending task', done: false },
      ],
    });
    const ev = parseEvent(line);
    assert.strictEqual(ev?.type, 'todo_update');
    const items = (ev as any).items;
    assert.strictEqual(items.length, 3);
    assert.strictEqual(items[0].done, true);
    assert.strictEqual(items[1].in_progress, true);
  });

  it('ignores unknown types', () => {
    const line = JSON.stringify({ type: 'future_event', payload: 1 });
    assert.strictEqual(parseEvent(line), undefined);
  });

  it('ignores invalid json', () => {
    assert.strictEqual(parseEvent('not json'), undefined);
  });
});

describe('format', () => {
  it('formatTodoHtml renders items with correct colors', () => {
    const html = formatTodoHtml([
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

  it('formatTodoHtml escapes HTML', () => {
    const html = formatTodoHtml([{ text: '<script>alert(1)</script>', done: false }]);
    assert.ok(!html.includes('<script>'), 'should escape HTML tags');
    assert.ok(html.includes('&lt;script&gt;'), 'should have escaped tags');
  });

  it('escapeHtml handles ampersands and angle brackets', () => {
    assert.strictEqual(escapeHtml('a < b & c > d'), 'a &lt; b &amp; c &gt; d');
  });

  it('truncate short strings unchanged', () => {
    assert.strictEqual(truncate('hello', 10), 'hello');
  });

  it('truncate long strings with ellipsis', () => {
    assert.strictEqual(truncate('abcdefghij', 5), 'abcde...');
  });
});

describe('bridge NDJSON format', () => {
  it('sendPrompt format is valid JSON', () => {
    const line = JSON.stringify({ type: 'prompt', text: 'hello world' });
    const parsed = JSON.parse(line);
    assert.strictEqual(parsed.type, 'prompt');
    assert.strictEqual(parsed.text, 'hello world');
  });

  it('sendApproval format is valid JSON', () => {
    const line = JSON.stringify({ type: 'approval', id: 'call_123', approved: true });
    const parsed = JSON.parse(line);
    assert.strictEqual(parsed.type, 'approval');
    assert.strictEqual(parsed.id, 'call_123');
    assert.strictEqual(parsed.approved, true);
  });
});