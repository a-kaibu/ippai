import assert from 'node:assert/strict';
import { test } from 'node:test';
import { partition } from './partition.mjs';

test('all probes run once, including short lists and uneven shards', () => {
  for (let n = 0; n < 23; n++) {
    const names = Array.from({ length: n }, (_, i) => `probe-${i}`);
    for (let count = 1; count <= 8; count++) {
      const shards = Array.from({ length: count }, (_, i) => partition(names, `${i}/${count}`));
      assert.deepEqual(shards.flat().sort(), [...names].sort());
      assert.equal(new Set(shards.flat()).size, n);
    }
    assert.deepEqual(partition(names), names);
  }
});

test('bad shard settings fail instead of silently skipping tests', () => {
  for (const shard of ['1/1', '-1/4', '0/0', '4/4', '0/1junk', 'foo']) {
    assert.throws(() => partition(['probe'], shard), /invalid SHARD/);
  }
});
