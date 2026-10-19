import assert from 'node:assert/strict';
import { test } from 'node:test';
import { Recall, recallPrompts } from './recall.ts';
import { rankSkills, skillQuery } from './skills.ts';
import { quoteForDraft } from '../transcript/selection.ts';

test('recall steps back and forth and ends on any other edit', () => {
  const r = new Recall();
  const prompts = ['newest', 'older', 'oldest'];
  assert.equal(r.up(prompts, 'typed'), null, 'only from an empty composer');
  assert.equal(r.up(prompts, ''), 'newest');
  assert.equal(r.up(prompts, 'newest'), 'older');
  assert.equal(r.up(prompts, 'older'), 'oldest');
  assert.equal(r.up(prompts, 'oldest'), null, 'no further back');
  assert.equal(r.down('oldest'), 'older');
  assert.equal(r.down('older'), 'newest');
  assert.equal(r.down('newest'), '', 'past the newest: empty again');
  assert.equal(r.active, false);
  r.up(prompts, '');
  assert.equal(r.up(prompts, 'newest, edited'), null, 'an edit ends the session');
  assert.equal(r.active, false);
});

test('recall prompts are the complete user messages, newest first, without repeats', () => {
  const rows = [
    { role: 'user', text: 'a', status: 'complete' },
    { role: 'assistant', text: 'x', status: 'complete' },
    { role: 'user', text: 'b', status: 'complete' },
    { role: 'user', text: 'b', status: 'complete' },
    { role: 'user', text: 'in flight', status: 'saved' },
  ];
  assert.deepEqual(recallPrompts(rows), ['b', 'a']);
});

test('skills complete from a bare "/prefix" and rank exact, prefix, word, substring, description', () => {
  assert.equal(skillQuery('/rev'), 'rev');
  assert.equal(skillQuery('/'), '');
  assert.equal(skillQuery('hi /rev'), null);
  assert.equal(skillQuery('/rev now'), null);
  const list = [
    { name: 'code-review', description: '' },
    { name: 'review', description: '' },
    { name: 'prereview', description: '' },
    { name: 'deploy', description: 'review then ship' },
    { name: 'reviewer', description: '' },
  ];
  assert.deepEqual(rankSkills(list, 'review').map((s) => s.name), ['review', 'reviewer', 'code-review', 'prereview', 'deploy']);
});

test('a quote goes into the draft as a block quote', () => {
  assert.equal(quoteForDraft('a\n\nb'), '> a\n>\n> b\n\n');
});

test('chatElements reads leniently and a missing key is shown', async () => {
  const { parseChatElements, isShown } = await import('../settings/chat/elements.ts');
  const e = parseChatElements({ tokens: 'hidden', timeline: 'bogus', other: 'hidden' });
  assert.deepEqual(e, { tokens: 'hidden' });
  assert.equal(isShown(e, 'tokens'), false);
  assert.equal(isShown(e, 'turnTime'), true);
  assert.deepEqual(parseChatElements(null), {});
});

test('toolbar accessories join the left group unless they ask for the right; the eye goes right', async () => {
  const { accessorySide } = await import('./slots.ts');
  assert.equal(accessorySide({ id: 'orchestrator' }), 'start');
  assert.equal(accessorySide({ id: 'panel-context' }), 'end', 'the side-panel eye sits with the actions');
  assert.equal(accessorySide({ id: 'panel-context', side: 'start' }), 'start', 'an explicit side wins');
  assert.equal(accessorySide({ id: 'x', side: 'end' }), 'end');
});
