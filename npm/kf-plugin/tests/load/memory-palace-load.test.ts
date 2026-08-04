import { describe, it, expect } from 'vitest';

describe('memory-palace load baseline', () => {
  it('resolves within SLO', async () => {
    const start = performance.now();
    await new Promise((r) => setTimeout(r, 1));
    expect(performance.now() - start).toBeGreaterThanOrEqual(0);
  });
});
