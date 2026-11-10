import { test } from 'node:test';
import assert from 'node:assert/strict';
import { estimateItem, formatElapsed, layout, type Item } from './workBlocks.ts';
import { ESTIMATE } from './virtual.ts';
import { row } from '../dev/fixtures.ts';
import { recapLine } from './recapText.ts';
import type { MessageRow } from '../api/types.ts';

function turn() {
  return [
    row({ id: 1, role: 'user', text: 'do it', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'let me look', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'toolResult', toolName: 'Bash', status: 'error', timestamp: 3000 }),
    row({ id: 5, role: 'assistant', text: 'done', timestamp: 65_000 }),
  ];
}

const shape = (items: Item[]) =>
  items.map((i) => (i.kind === 'row' ? i.row.id : `${i.kind}[${i.rows.map((r) => r.id).join(',')}]`));

test('a settled turn folds its tool calls, keeping notes, errors and the final reply', () => {
  const items = layout(turn(), false);
  assert.deepEqual(shape(items), [1, 'fold[3]', 2, 4, 5]);
  const fold = items.find((i) => i.kind === 'fold');
  // Timed from the run's first output (1 s), not from the prompt (0 s).
  assert.equal(fold?.kind === 'fold' && fold.elapsedMs, 64_000);
});

test('a turn with no tool call and no thinking does not fold, however long it took to start', () => {
  const rows = [
    row({ id: 1, role: 'user', text: 'Hello', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: '', timestamp: 30_000 }),
    row({ id: 3, role: 'assistant', text: 'Hi! What are we building?', timestamp: 31_000 }),
  ];
  // The empty assistant message in between is not drawn: an empty row would just be a gap.
  assert.deepEqual(shape(layout(rows, false)), [1, 3]);
});

test('thinking alone is work: the turn folds, timed from its first output', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', thinking: 'weighing it up', timestamp: 30_000 }),
    row({ id: 3, role: 'assistant', text: 'answer', timestamp: 42_000 }),
  ];
  const items = layout(rows, false);
  assert.deepEqual(shape(items), [1, 'fold[2]', 3]);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.elapsedMs, 12_000);
});

test('the row right after a closed fold is marked, so it draws no second rule', () => {
  const items = layout(turn(), false);
  const flag = (id: number) => {
    const it = items.find((i) => i.kind === 'row' && i.row.id === id);
    return it?.kind === 'row' && it.afterFold;
  };
  assert.equal(flag(2), true);
  assert.equal(flag(4), false);
  const plain = layout([row({ id: 1, role: 'user' }), row({ id: 2, role: 'toolResult' }), row({ id: 3, role: 'assistant', text: 'done' })], false);
  const done = plain.find((i) => i.kind === 'row' && i.row.id === 3);
  assert.equal(done?.kind === 'row' && done.final && done.afterFold, true);
  // Opened, the work sits between the fold and the reply, so the reply keeps its hairline.
  const open = layout(turn(), false, new Set(['k1']));
  const reply = open.find((i) => i.kind === 'row' && i.row.id === 5);
  assert.equal(reply?.kind === 'row' && reply.afterFold, false);
});

test('a live turn folds as it goes, so settling changes nothing', () => {
  const live = layout(turn(), true);
  const settled = layout(turn(), false);
  assert.deepEqual(shape(live), [1, 'fold[3]', 2, 4, 5]);
  assert.deepEqual(shape(live), shape(settled));
  // The reply being written after the last tool already has its final look.
  const flags = (items: Item[]) => items.map((i) => (i.kind === 'row' ? [i.final, i.afterFold, i.afterTool] : 'fold'));
  assert.deepEqual(flags(live), flags(settled));
});

test('while a tool is the newest row, it is the open part and the work before it folds', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'let me look', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'assistant', text: 'now the tests', timestamp: 3000 }),
    row({ id: 5, role: 'toolResult', toolName: 'Bash', status: 'streaming', timestamp: 4000 }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 'fold[3]', 2, 4, 5]);
  // Text written after it opens instead, and the tool folds.
  const more = [...rows.slice(0, 4), { ...rows[4]!, status: 'complete' as const }, row({ id: 6, role: 'assistant', text: 'All', timestamp: 5000 })];
  assert.deepEqual(shape(layout(more, true)), [1, 'fold[3,5]', 2, 4, 6]);
});

test('live, nothing folds until there is visible work to fold (no "Worked for 0s" over the first tool)', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    // The empty message the host saves a tool call under.
    row({ id: 2, role: 'assistant', text: '', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Bash', status: 'streaming', timestamp: 1000 }),
  ];
  // The empty message above the running call is not drawn, so the call sits under the prompt
  // with no gap; once real work can fold, it folds as usual.
  assert.deepEqual(shape(layout(rows, true)), [1, 3]);
  const second = [...rows, row({ id: 4, role: 'assistant', text: '', timestamp: 2000 }), row({ id: 5, role: 'toolResult', toolName: 'Bash', status: 'streaming', timestamp: 2000 })];
  assert.deepEqual(shape(layout(second, true)), [1, 'fold[2,3,4]', 5]);
});

test('rows with nothing to show are never drawn, so hidden tool activity leaves no empty gap', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    // The empty messages the host saves the two tool calls under.
    row({ id: 2, role: 'assistant', text: '', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Bash', timestamp: 2000 }),
    row({ id: 4, role: 'assistant', text: 'let me check', timestamp: 3000 }),
    row({ id: 5, role: 'assistant', text: '', timestamp: 4000 }),
    row({ id: 6, role: 'toolResult', toolName: 'Read', timestamp: 5000 }),
    row({ id: 7, role: 'assistant', text: 'done', timestamp: 6000 }),
  ];
  // Closed: the calls and their empty messages fold behind one compact row; the step count is
  // the two calls, not each call counted twice.
  const closed = layout(rows, false);
  assert.deepEqual(shape(closed), [1, 'fold[2,3,5,6]', 4, 7]);
  const fold = closed.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.steps, 2);
  // Opened: the calls are listed, the empty messages take no room between them, and the note
  // still tightens above the call that follows it across its empty message.
  const opened = layout(rows, false, new Set(['k1']));
  assert.deepEqual(shape(opened), [1, 'fold[2,3,5,6]', 3, 4, 6, 7]);
  const note = opened.find((i) => i.kind === 'row' && i.row.id === 4);
  assert.equal(note?.kind === 'row' && note.beforeTool, true, 'note id4 is followed by empty id5 then tool id6');
  // Live, before anything real can fold, the running call sits under the prompt with no empty row.
  const live = [
    rows[0]!,
    { ...rows[1]!, status: 'streaming' as const },
    { ...rows[2]!, status: 'streaming' as const },
  ];
  assert.deepEqual(shape(layout(live, true)), [1, 3]);
  // A turn whose whole reply is empty messages shows the prompt and draws nothing else.
  assert.deepEqual(shape(layout([row({ id: 1, role: 'user', timestamp: 0 }), row({ id: 2, role: 'assistant', text: '', thinking: '  ', timestamp: 1000 })], false)), [1]);
});

test('a live turn that only thought folds its thinking behind the newest row', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: '', thinking: 'hmm', timestamp: 1000 }),
    row({ id: 3, role: 'assistant', text: 'Here', timestamp: 2000 }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 'fold[2]', 3]);
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[2]', 3]);
});

test('Agent calls stay in the flow, side by side as one group, whether they run or have finished', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'toolResult', toolName: 'Agent', toolCallId: 'a1', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'toolResult', toolName: 'Task', toolCallId: 'a2', timestamp: 3000 }),
    row({ id: 5, role: 'assistant', text: 'Started two reviewers.', timestamp: 4000 }),
    row({ id: 6, role: 'toolResult', toolName: 'Agent', toolCallId: 'a3', timestamp: 5000 }),
  ];
  // The Read between the first two folds away, so the two calls are neighbours; the note splits the third off.
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[3]', 'agents[2,4]', 5, 'agents[6]']);
  assert.deepEqual(shape(layout(rows, true)), shape(layout(rows, false)));
});

test('an earlier turn folds while the latest is live; expanding unfolds it', () => {
  const rows = [
    ...turn(),
    row({ id: 6, role: 'user', timestamp: 70_000 }),
    row({ id: 7, role: 'toolResult', timestamp: 71_000 }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 'fold[3]', 2, 4, 5, 6, 7]);
  // Opened: the "Worked for" row stays (so it can be closed again) and the work follows in order.
  assert.deepEqual(shape(layout(rows, true, new Set(['k1']))), [1, 'fold[3]', 2, 3, 4, 5, 6, 7]);
});

test('context, retry and interrupted rows stay visible', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'context', text: 'compacted' }),
    row({ id: 3, role: 'assistant', text: 'x', details: { retry: { state: 'recovered', attempt: 1 } } }),
    row({ id: 4, role: 'toolResult' }),
    row({ id: 8, role: 'toolResult', status: 'interrupted' }),
    row({ id: 5, role: 'assistant', text: 'reply' }),
  ];
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[4]', 2, 3, 8, 5]);
});

test("only the newest notice is a live region, and only while it is the transcript's last row", () => {
  const live = (items: Item[]) => items.flatMap((i) => (i.kind === 'row' && i.noticeLive ? [i.row.id] : []));
  const notice = (id: number) => row({ id, role: 'context', text: 'Model could not load' });
  // The transcript ends on the notice: it is the live one.
  assert.deepEqual(live(layout([row({ id: 1, role: 'user' }), notice(2)], false)), [2]);
  // Once anything follows it, it is just history: no live region anywhere.
  assert.deepEqual(live(layout([row({ id: 1, role: 'user' }), notice(2), row({ id: 3, role: 'assistant', text: 'reply' })], false)), []);
  assert.deepEqual(live(layout([notice(1), row({ id: 2, role: 'user' }), notice(3), row({ id: 4, role: 'user' })], false)), []);
  // Other context rows are never live regions, even at the end.
  assert.deepEqual(live(layout([row({ id: 1, role: 'user' }), row({ id: 2, role: 'context', text: 'switched model', details: { contextKind: 'modelChange' } })], false)), []);
});

test('formatElapsed', () => {
  assert.equal(formatElapsed(4_400), '4s');
  assert.equal(formatElapsed(65_000), '1m 5s');
  assert.equal(formatElapsed(3_720_000), '1h 2m');
});

test('a steer read mid-turn stays in the turn and is never folded, live or settled', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'toolResult' }),
    row({ id: 3, role: 'user', text: 'also this', details: { mode: 'steer' } }),
    row({ id: 4, role: 'toolResult' }),
    row({ id: 5, role: 'assistant', text: 'reply' }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 'fold[2,4]', 3, 5]);
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[2,4]', 3, 5]);
});

test('the fold counts failed tool calls and flags the final answer', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'toolResult', toolName: 'Bash', details: { isError: true } }),
    row({ id: 3, role: 'toolResult', toolName: 'Read' }),
    row({ id: 4, role: 'toolResult', toolName: 'Bash', details: { isError: true } }),
    row({ id: 5, role: 'assistant', text: 'done' }),
  ];
  const items = layout(rows, false);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.failed, 2);
  const reply = items.find((i) => i.kind === 'row' && i.row.id === 5);
  assert.equal(reply?.kind === 'row' && reply.final, true);
  assert.equal(reply?.kind === 'row' && reply.afterTool, true);
  // A plain answer with no tool work has no hairline.
  const plain = layout([row({ id: 1, role: 'user' }), row({ id: 2, role: 'assistant', text: 'hi' })], false);
  const hi = plain.find((i) => i.kind === 'row' && i.row.id === 2);
  assert.equal(hi?.kind === 'row' && hi.final, false);
});

test('opened work is indented, the shown rows are not', () => {
  const items = layout(turn(), false, new Set(['k1']));
  const ind = (id: number) => {
    const it = items.find((i) => i.kind === 'row' && i.row.id === id);
    return it?.kind === 'row' && it.indent;
  };
  assert.equal(ind(3), true);
  assert.equal(ind(2), false);
  assert.equal(ind(5), false);
});

test('estimateItem guesses by kind and length, and remembers each row', () => {
  const short = row({ id: 1, role: 'assistant', text: 'ok' });
  const long = row({ id: 2, role: 'assistant', text: 'word '.repeat(2000) });
  const items: Item[] = [
    { kind: 'row', row: short },
    { kind: 'row', row: long },
  ];
  assert.ok(estimateItem(items[1]) > estimateItem(items[0]) * 5);
  assert.equal(estimateItem(undefined), ESTIMATE);
  // The same row object gives the same answer without re-reading its text.
  const frozen = { ...long };
  const first = estimateItem({ kind: 'row', row: frozen });
  Object.defineProperty(frozen, 'text', { get: () => assert.fail('text read twice') });
  assert.equal(estimateItem({ kind: 'row', row: frozen }), first);
});

test('thinking behind a closed fold is hidden on the notes that carry it, and shown once opened', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'note', thinking: 'why', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'assistant', text: 'answer', thinking: 'wrap up', timestamp: 3000 }),
  ];
  const hidden = (items: Item[]) => items.flatMap((i) => (i.kind === 'row' && i.hideThinking ? [i.row.id] : []));
  assert.deepEqual(hidden(layout(rows, false)), [2, 4]);
  assert.deepEqual(hidden(layout(rows, false, new Set(['k1']))), []);
  // The step count is tool calls and thinking blocks, not the final reply's thinking.
  const fold = layout(rows, false).find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.steps, 2);
  // Opened, the note above a tool row tightens its bottom padding; the reply does not.
  const tight = layout(rows, false, new Set(['k1'])).flatMap((i) => (i.kind === 'row' && i.beforeTool ? [i.row.id] : []));
  assert.deepEqual(tight, [2]);
  // Live, the part still being written keeps its thinking in view.
  assert.deepEqual(hidden(layout(rows, true)), [2]);
  // A turn that only thought and answered has nothing to fold: the thinking stays where it is.
  assert.deepEqual(hidden(layout([rows[0]!, { ...rows[3]!, id: 2 }], false)), []);
});

test('a gallery or board never folds, and a settled turn with a checklist or three steps gets a recap', () => {
  const todo = (done: number) =>
    JSON.stringify({ todos: ['one', 'two', 'three'].map((content, i) => ({ content, status: i < done ? 'completed' : 'pending' })) });
  const gallery = row({ id: 4, role: 'toolResult', toolName: 'mcp__helm__app_show_images', text: '{"galleryId":"g1","count":3}', timestamp: 4000 });
  const board = row({ id: 5, role: 'toolResult', toolName: 'mcp__helm__app_offer_options', text: '{"boardId":"b1","next":"wait"}', timestamp: 5000 });
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'toolResult', toolName: 'TodoWrite', details: { toolArgs: todo(0) }, timestamp: 2000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Edit', details: { toolArgs: '{"file_path":"a.ts"}' }, timestamp: 3000 }),
    gallery,
    board,
    row({ id: 6, role: 'toolResult', toolName: 'Edit', details: { toolArgs: '{"file_path":"b.ts"}' }, timestamp: 6000 }),
    row({ id: 7, role: 'toolResult', toolName: 'TodoWrite', details: { toolArgs: todo(2) }, timestamp: 7000 }),
    row({ id: 8, role: 'assistant', text: 'done', timestamp: 125_000 }),
  ];
  const items = layout(rows, false);
  // The cards stay in the flow; the plain tool rows fold.
  assert.deepEqual(shape(items), [1, 'fold[2,3,6,7]', 4, 5, 8]);
  const reply = items.find((i) => i.kind === 'row' && i.row.id === 8);
  const recap = reply?.kind === 'row' ? reply.recap : undefined;
  assert.equal(recap?.files, 2);
  assert.deepEqual(recap?.galleries, [{ galleryId: 'g1', count: 3 }]);
  assert.equal(recap?.checklist?.filter((t) => t.state === 'completed').length, 2);
  assert.equal(recap && recapLine(recap), 'Done in 2m 3s · 2 of 3 steps · 2 files edited · 3 pictures');
  // Only the final reply carries it, and not while the turn is still running.
  assert.equal(items.filter((i) => i.kind === 'row' && i.recap).length, 1);
  assert.equal(layout(rows, true).some((i) => i.kind === 'row' && i.recap), false);
  // Without a checklist it takes three tool steps.
  const bare = (n: number) => [
    row({ id: 1, role: 'user', timestamp: 0 }),
    ...Array.from({ length: n }, (_, i) => row({ id: 2 + i, role: 'toolResult', toolName: 'Read', timestamp: 1000 * (i + 1) })),
    row({ id: 20, role: 'assistant', text: 'ok', timestamp: 9000 }),
  ];
  const hasRecap = (rs: MessageRow[]) => layout(rs, false).some((i) => i.kind === 'row' && i.recap !== undefined);
  assert.deepEqual([hasRecap(bare(2)), hasRecap(bare(3))], [false, true]);
});
