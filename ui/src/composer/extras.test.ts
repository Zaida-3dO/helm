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
  // Hiding matches case-insensitively; one of Helm's own plugin skills also hides on its bare
  // folder name, the CLI showing it under the plugin prefix.
  const { matchesHidden } = await import('./skills.ts');
  const helmSkill = { name: 'helm:review', description: '', scope: 'helm-skill' };
  assert.equal(matchesHidden(helmSkill, ['Review']), true, 'the bare folder name, any case');
  assert.equal(matchesHidden(helmSkill, ['helm:review']), true, 'the reported name too');
  assert.equal(matchesHidden(helmSkill, ['reviewer']), false, 'a different name stays');
  assert.equal(matchesHidden({ name: 'helm:deploy', description: '', scope: 'plugin' }, ['deploy']), false, 'only Helm’s own drop the prefix');
  assert.equal(matchesHidden({ name: 'deploy', description: '' }, ['deploy']), true);
  assert.equal(matchesHidden({ name: 'deploy', description: '' }, [' deploy ']), true, 'spaces round the stored name');
});

test('a long paste becomes a named file, a rich paste keeps its external links, anything odd stays plain', async () => {
  const { isLongPaste, lineCount, pastedFileName, linkedPaste } = await import('./paste.ts');
  assert.deepEqual([lineCount(''), lineCount('a'), lineCount('a\nb\n'), lineCount('\n'.repeat(249))], [0, 1, 2, 249]);
  for (const [text, long] of [
    ['x'.repeat(20_000), true],
    ['x'.repeat(19_999), false],
    ['a\n'.repeat(250), true],
    ['a\n'.repeat(249), false],
  ] as const) {
    assert.equal(isLongPaste(text), long, `${text.length} chars`);
  }
  const diff = 'diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n';
  for (const [text, name] of [
    [diff, 'pasted-6-lines.diff'],
    [JSON.stringify({ a: [1, 2] }), 'pasted-1-line.json'],
    ['[not json\nat all', 'pasted-2-lines.txt'],
    ['a\nb', 'pasted-2-lines.txt'],
  ] as const) {
    assert.equal(pastedFileName(text), name);
  }
  for (const [plain, html, want] of [
    ['See the docs now', '<p>See the <a href="https://example.com/a">docs</a> now</p>', 'See the [docs](https://example.com/a) now'],
    ['one two', '<a href="https://a.io">one</a> <a href="https://b.io">two</a>', '[one](https://a.io) [two](https://b.io)'],
    ['a\nb', '<p>a</p><p><a href="https://x.io">b</a></p>', 'a\n[b](https://x.io)'],
    ['Tom & Jerry', '<a href="https://e.com/?a=1&amp;b=2">Tom &amp; Jerry</a>', '[Tom & Jerry](https://e.com/?a=1&b=2)'],
    ['a [b] c', '<a href="https://e.com">a [b] c</a>', '[a \\[b\\] c](https://e.com)'],
    ['Hi there', '<script>evil()</script><p>Hi <a href="http://a.io/x_y(1)">there</a></p>', 'Hi [there](http://a.io/x_y%281%29)'],
    // left as plain text: the HTML shows other text, the label is the address, the link is not an external web address, no link
    ['Hi there', '<p>Hello <a href="https://a.io">there</a></p>', null],
    ['https://example.com', '<a href="https://example.com">https://example.com</a>', null],
    ['x', '<a href="javascript:alert(1)">x</a>', null],
    ['x', '<a href="/relative">x</a>', null],
    ['plain', '<p>plain</p>', null],
    ['plain', '', null],
  ] as const) {
    assert.equal(linkedPaste(plain, html), want, html);
  }
});

test('an @ mention is the word at the caret, and a pick writes its path', async () => {
  const { mentionAt, mentionText, applyMention } = await import('./mentions.ts');
  for (const [text, caret, want] of [
    ['look at @src/ma', 15, { start: 8, end: 15, query: 'src/ma' }],
    ['fix @abc def', 7, { start: 4, end: 8, query: 'ab' }],
    ['@a', 2, { start: 0, end: 2, query: 'a' }],
    ['mail me@x.io', 12, null],
    ['@@a', 3, null],
    ['no mention', 4, null],
    ['@a', 9, null],
  ] as const) {
    assert.deepEqual(mentionAt(text, caret), want, `${text}|${caret}`);
  }
  const file = { path: 'C:\\p\\src\\a.ts', label: 'src\\a.ts', kind: 'file' as const };
  assert.equal(mentionText(file, false), '@src/a.ts');
  assert.equal(mentionText(file, true), '@C:/p/src/a.ts');
  assert.equal(mentionText({ path: 'C:\\p\\src', label: 'src', kind: 'dir' }, false), '@src/');
  assert.equal(mentionText({ path: 'C:\\p\\my docs\\a.md', label: 'my docs\\a.md', kind: 'file' }, false), '@"my docs/a.md"');
  assert.deepEqual(applyMention('look at @sr now', { start: 8, end: 11, query: 'sr' }, '@src/a.ts', false), { text: 'look at @src/a.ts now', caret: 18 });
  assert.deepEqual(applyMention('@sr', { start: 0, end: 3, query: 'sr' }, '@src/a.ts', false), { text: '@src/a.ts ', caret: 10 });
  assert.deepEqual(applyMention('@sr', { start: 0, end: 3, query: 'sr' }, '@src/', true), { text: '@src/', caret: 5 });
});

test('the composer waits for the first runtime check, and the scroll-on-send setting is on unless turned off', async () => {
  const { runtimeCheck } = await import('./runtimeCheck.ts');
  const { parseScrollOnSend } = await import('./scrollOnSend.ts');
  assert.equal(runtimeCheck.get().checked, false, 'nothing has answered yet');
  await runtimeCheck.refresh(async () => ({ node: { path: 'node' }, claude: { path: '' }, problems: ['Claude Code was not found'] }));
  assert.deepEqual(runtimeCheck.get(), { blocked: true, problems: ['Claude Code was not found'], checked: true });
  for (const [stored, on] of [[null, true], [undefined, true], [true, true], [false, false], [0, true]] as const) {
    assert.equal(parseScrollOnSend(stored), on, String(stored));
  }
});
