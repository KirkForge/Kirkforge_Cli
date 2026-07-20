import { describe, it } from 'node:test';
import * as assert from 'assert';
import { parseEvent } from '../protocol';

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

  it('ignores unknown types', () => {
    const line = JSON.stringify({ type: 'future_event', payload: 1 });
    assert.strictEqual(parseEvent(line), undefined);
  });

  it('ignores invalid json', () => {
    assert.strictEqual(parseEvent('not json'), undefined);
  });
});
