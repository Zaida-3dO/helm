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

test('the / menu stays open with the chosen command while its arguments are typed', async () => {
  const { commandWithArgs, moveActive, skillMenu } = await import('./skills.ts');
  const list = [
    { name: 'review', description: 'Review the diff' },
    { name: 'reviewer', description: 'A reviewer' },
    { name: 'deploy', description: 'Ship it' },
  ];
  assert.equal(commandWithArgs('/review the diff'), 'review');
  assert.equal(commandWithArgs('/review'), null, 'no arguments yet');
  assert.equal(commandWithArgs('please /review x'), null);
  assert.deepEqual(skillMenu('/rev', list)?.items.map((s) => s.name), ['review', 'reviewer']);
  assert.equal(skillMenu('/rev', list)?.mode, 'pick');
  const args = skillMenu('/review the auth module', list);
  assert.equal(args?.mode, 'args');
  assert.deepEqual(args?.items, [{ name: 'review', description: 'Review the diff' }], 'with its description');
  assert.equal(skillMenu('/unknown thing', list), null, 'an unknown command closes it');
  assert.equal(skillMenu('/zzz', list), null, 'no matches closes it');
  assert.equal(skillMenu('hello', list), null);
  // Helm's own commands lead the list, shadow a CLI skill of the same name, and are found however
  // they are spaced; only a whole draft naming one runs it.
  const { helmCommand, withHelmCommands, SCOPE_LABEL } = await import('./skills.ts');
  const all = withHelmCommands([...list, { name: 'roll-over', description: 'a CLI skill' }]);
  assert.deepEqual(all.map((s) => s.name), ['rollover', 'review', 'reviewer', 'deploy']);
  assert.equal(SCOPE_LABEL[all[0]!.scope!], 'Helm command');
  for (const typed of ['/roll', '/rollover', '/roll-over', '/roll-o', '/roll over', '/Roll Over', '/roll ']) {
    assert.deepEqual(skillMenu(typed, all)?.items.map((s) => s.name), ['rollover'], typed);
  }
  assert.equal(skillMenu('/review the diff', all)?.mode, 'args', 'a spaced CLI command is untouched');
  for (const sent of ['/rollover', '/rollover ', ' /roll-over', '/Roll over']) assert.equal(helmCommand(sent)?.name, 'rollover', sent);
  for (const sent of ['/roll', '/rollover now', 'rollover', 'please /rollover']) assert.equal(helmCommand(sent), null, sent);
  assert.equal(moveActive(0, 3, -1), 2);
  assert.equal(moveActive(2, 3, 1), 0);
  assert.equal(moveActive(0, 0, 1), -1);
});
