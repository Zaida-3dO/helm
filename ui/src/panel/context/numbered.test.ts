import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ReviewComment } from '../../api/panel.ts';
import { numbered } from './numbered.ts';

const c = (id: string, quote: string, body: string): ReviewComment => ({
  id, path: 'src/a.ts', diffSide: 'unstaged', side: 'right', startLine: 2, endLine: 3, quote, body, createdAt: 0,
});

test('comments become a numbered list with each quote fenced longer than its backticks', () => {
  const text = numbered([c('1', 'plain', 'Rename this.'), c('2', 'has ```fence``` inside', 'Why?')]);
  const lines = text.split('\n');
  assert.equal(lines[0], 'Please address these review comments:');
  assert.ok(text.includes('1. src/a.ts (working tree, new lines 2–3)'));
  assert.ok(text.includes('2. src/a.ts'));
  assert.ok(text.includes('```\nplain\n```'));
  assert.ok(text.includes('````\nhas ```fence``` inside\n````'), 'four backticks beat the run of three');
  assert.ok(text.endsWith('Why?'));
});
