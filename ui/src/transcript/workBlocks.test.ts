import { test } from 'node:test';
import assert from 'node:assert/strict';
import { estimateItem, foldLabel, formatElapsed, layout, revealKey, type Item } from './workBlocks.ts';
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
  items.map((i) =>
    i.kind === 'row'
      ? i.row.id
      : 'rows' in i
        ? `${i.kind}[${i.rows.map((r: { id: number }) => r.id).join(',')}]`
        : i.kind,
  );

test('a settled turn folds its tool calls into one group, keeping notes and the final reply; a failed call folds like a passed one', () => {
  const items = layout(turn(), false);
  // The failed call is not its own visible row any more: it sits inside the group, counted there.
  assert.deepEqual(shape(items), [1, 2, 'fold[3,4]', 5]);
  const fold = items.find((i) => i.kind === 'fold');
  // Timed over the run itself: the two calls at 2 s and 3 s.
  assert.equal(fold?.kind === 'fold' && fold.elapsedMs, 1000);
  assert.equal(fold?.kind === 'fold' && fold.failed, 1);
  assert.equal(fold?.kind === 'fold' && fold.tools, 2);
});

test('a turn with no tool call and no thinking does not fold, however long it took to start', () => {
  const rows = [
    row({ id: 1, role: 'user', text: 'Hello', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: '', timestamp: 30_000 }),
    row({ id: 3, role: 'assistant', text: 'Hi! What are we building?', timestamp: 31_000 }),
  ];
  // The empty assistant message in between is not drawn: an empty row would just be a gap.
  assert.deepEqual(shape(layout(rows, false)), [1, 3]);
  // A voice chat's "[quiet]" answer is never drawn, and the reply before it stays the final one.
  const quiet = [...turn(), row({ id: 6, role: 'assistant', text: '[quiet]', timestamp: 66_000 })];
  const items = layout(quiet, false);
  assert.deepEqual(shape(items), [1, 2, 'fold[3,4]', 5]);
  assert.equal(items.find((i) => i.kind === 'row' && i.row.id === 5 && i.final) !== undefined, true);
});

test('thinking alone is work: the turn folds it, reachable, not hidden', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', thinking: 'weighing it up', timestamp: 30_000 }),
    row({ id: 3, role: 'assistant', text: 'answer', timestamp: 42_000 }),
  ];
  const items = layout(rows, false);
  assert.deepEqual(shape(items), [1, 'fold[2]', 3]);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.thoughts, 1);
  // Opened, the thinking row itself appears, under the group's rule.
  const opened = layout(rows, false, new Set([revealKey('k1')]));
  assert.deepEqual(shape(opened), [1, 'fold[2]', 2, 3]);
  const thought = opened.find((i) => i.kind === 'row' && i.row.id === 2);
  assert.ok(thought?.kind === 'row' && thought.indent, 'the opened group lists its rows, indented');
});

test('the row right after a closed group is marked, so it draws no second rule', () => {
  const items = layout(turn(), false);
  const flag = (id: number) => {
    const it = items.find((i) => i.kind === 'row' && i.row.id === id);
    return it?.kind === 'row' && it.afterFold;
  };
  assert.equal(flag(2), false, 'the note sits above its following group, not below one');
  assert.equal(flag(5), true, 'the reply after the closed group needs no second rule');
  const plain = layout([row({ id: 1, role: 'user' }), row({ id: 2, role: 'toolResult' }), row({ id: 3, role: 'assistant', text: 'done' })], false);
  const done = plain.find((i) => i.kind === 'row' && i.row.id === 3);
  assert.equal(done?.kind === 'row' && done.final && done.afterFold, true);
  // Opened, the work sits between the group row and the reply, so the reply keeps its hairline.
  const open = layout(turn(), false, new Set([revealKey('k1')]));
  const reply = open.find((i) => i.kind === 'row' && i.row.id === 5);
  assert.equal(reply?.kind === 'row' && reply.afterFold, false);
});

test('a live turn folds as it goes, so settling changes nothing', () => {
  const live = layout(turn(), true);
  const settled = layout(turn(), false);
  assert.deepEqual(shape(live), [1, 2, 'fold[3,4]', 5]);
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
  assert.deepEqual(shape(layout(rows, true)), [1, 2, 'fold[3]', 4, 5]);
  // Text written after it opens instead; the note splits the finished run off as its own group.
  const more = [...rows.slice(0, 4), { ...rows[4]!, status: 'complete' as const }, row({ id: 6, role: 'assistant', text: 'All', timestamp: 5000 })];
  assert.deepEqual(shape(layout(more, true)), [1, 2, 'fold[3]', 4, 'fold[5]', 6]);
});

test('live, nothing folds until there is visible work to fold (no empty header over the first tool)', () => {
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

test('rows with nothing to show are never drawn, so grouped tool activity leaves no empty gap', () => {
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
  // Closed: the note splits the run, so each call folds into its own compact row; the counts are
  // the calls themselves, not each call's empty message.
  const closed = layout(rows, false);
  assert.deepEqual(shape(closed), [1, 'fold[2,3]', 4, 'fold[5,6]', 7]);
  const folds = closed.filter((i) => i.kind === 'fold');
  assert.deepEqual(folds.map((f) => (f.kind === 'fold' ? f.tools : 0)), [1, 1]);
  // Opened: the calls are listed, the empty messages take no room between them, and the note
  // still tightens above the call that follows it across its empty message.
  const opened = layout(rows, false, new Set([revealKey('k1')]));
  assert.deepEqual(shape(opened), [1, 'fold[2,3]', 3, 4, 'fold[5,6]', 6, 7]);
  const note = opened.find((i) => i.kind === 'row' && i.row.id === 4);
  assert.equal(note?.kind === 'row' && note.beforeTool, true, 'note id4 is followed by group row then tool id6');
  // Only the opened group lists its rows: opening one leaves the other folded.
  const oneOpen = layout(rows, false, new Set(['cg:k1:2']));
  assert.deepEqual(shape(oneOpen), [1, 'fold[2,3]', 3, 4, 'fold[5,6]', 7]);
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

test('a live turn that only thought folds its thinking behind its own row', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: '', thinking: 'hmm', timestamp: 1000 }),
    row({ id: 3, role: 'assistant', text: 'Here', timestamp: 2000 }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 'fold[2]', 3]);
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[2]', 3]);
});

test('Agent calls stay in the flow, side by side as one group, even when a folded call sat between them', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'toolResult', toolName: 'Agent', toolCallId: 'a1', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'toolResult', toolName: 'Task', toolCallId: 'a2', timestamp: 3000 }),
    row({ id: 5, role: 'assistant', text: 'Started two reviewers.', timestamp: 4000 }),
    row({ id: 6, role: 'toolResult', toolName: 'Agent', toolCallId: 'a3', timestamp: 5000 }),
  ];
  // The Read between the first two folds away, so the two calls are neighbours and the folded
  // group's row moves ahead of them; the note splits the third off.
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[3]', 'agents[2,4]', 5, 'agents[6]']);
  assert.deepEqual(shape(layout(rows, true)), shape(layout(rows, false)));
  // Opened, the Read is on screen between the two cards, so they are not one group.
  assert.deepEqual(shape(layout(rows, false, new Set([revealKey('k1')]))), [1, 'agents[2]', 'fold[3]', 3, 'agents[4]', 5, 'agents[6]']);
});

test('an earlier turn folds while the latest is live; opening the turn unfolds its work', () => {
  const rows = [
    ...turn(),
    row({ id: 6, role: 'user', timestamp: 70_000 }),
    row({ id: 7, role: 'toolResult', timestamp: 71_000 }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 2, 'fold[3,4]', 5, 6, 7]);
  // Opened: the group row stays (so it can be closed again) and the work follows in order.
  assert.deepEqual(shape(layout(rows, true, new Set([revealKey('k1')]))), [1, 2, 'fold[3,4]', 3, 4, 5, 6, 7]);
});

test('context, retry and interrupted rows stay visible; an interrupted call is not folded away', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'context', text: 'compacted' }),
    row({ id: 3, role: 'assistant', text: 'x', details: { retry: { state: 'recovered', attempt: 1 } } }),
    row({ id: 4, role: 'toolResult' }),
    row({ id: 8, role: 'toolResult', status: 'interrupted' }),
    row({ id: 5, role: 'assistant', text: 'reply' }),
  ];
  assert.deepEqual(shape(layout(rows, false)), [1, 2, 3, 'fold[4]', 8, 5]);
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

test('foldLabel says what the group holds, and how long it took', () => {
  assert.equal(foldLabel({ tools: 12, thoughts: 0, messages: 0, elapsedMs: 40_000 }), '12 tool calls · worked for 40s');
  assert.equal(foldLabel({ tools: 1, thoughts: 0, messages: 0, elapsedMs: 1000 }), '1 tool call · worked for 1s');
  assert.equal(foldLabel({ tools: 0, thoughts: 2, messages: 1, elapsedMs: 0 }), '2 thoughts · 1 message');
  assert.equal(foldLabel({ tools: 3, thoughts: 1, messages: 2, elapsedMs: 65_000 }), '3 tool calls · 1 thought · 2 messages · worked for 1m 5s');
});

test('a steer read mid-turn stays in the turn and is never folded, live or settled', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'toolResult' }),
    row({ id: 3, role: 'user', text: 'also this', details: { mode: 'steer' } }),
    row({ id: 4, role: 'toolResult' }),
    row({ id: 5, role: 'assistant', text: 'reply' }),
  ];
  // The steer is the person's own words: it stands on its own and splits the run into two groups.
  assert.deepEqual(shape(layout(rows, true)), [1, 'fold[2]', 3, 'fold[4]', 5]);
  assert.deepEqual(shape(layout(rows, false)), [1, 'fold[2]', 3, 'fold[4]', 5]);
});

test('message cards from another chat, a schedule or the app fold into the group they sit in', () => {
  const peer = (id: number) =>
    row({
      id,
      role: 'user',
      text: `[Message from another chat: "Coordinator" (chat-9)]\nSent by another Helm chat.\n\nSteer message body.`,
      details: { origin: 'other_chat', mode: 'steer' },
    });
  const fromHelm = (id: number) => row({ id, role: 'user', text: 'Forward the port when the bridge is up.', details: { origin: 'app', mode: 'steer' } });
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'toolResult', toolName: 'Bash', timestamp: 1000 }),
    peer(3),
    row({ id: 4, role: 'toolResult', toolName: 'Read', timestamp: 3000 }),
    fromHelm(5),
    row({ id: 6, role: 'assistant', text: 'reply', timestamp: 5000 }),
  ];
  const items = layout(rows, false);
  // Two calls and both cards make one group: the message cards count as steps toward the label.
  assert.deepEqual(shape(items), [1, 'fold[2,3,4,5]', 6]);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.tools, 2);
  assert.equal(fold?.kind === 'fold' && fold.messages, 2);
  // Opened, the cards appear between the calls, each one visible.
  const opened = layout(rows, false, new Set([revealKey('k1')]));
  assert.deepEqual(shape(opened), [1, 'fold[2,3,4,5]', 2, 3, 4, 5, 6]);
  assert.deepEqual(
    opened.flatMap((i) => (i.kind === 'row' && i.indent ? [i.row.id] : [])),
    [2, 3, 4, 5],
  );
});

test('the group counts failed tool calls and flags the final answer', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'toolResult', toolName: 'Bash', details: { isError: true } }),
    row({ id: 3, role: 'toolResult', toolName: 'Read' }),
    row({ id: 4, role: 'toolResult', toolName: 'Bash', details: { isError: true } }),
    row({ id: 5, role: 'assistant', text: 'done' }),
  ];
  const items = layout(rows, false);
  // Passed and failed calls sit in the same group; the failure shows as a count on the group row.
  assert.deepEqual(shape(items), [1, 'fold[2,3,4]', 5]);
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
  const items = layout(turn(), false, new Set([revealKey('k1')]));
  const ind = (id: number) => {
    const it = items.find((i) => i.kind === 'row' && i.row.id === id);
    return it?.kind === 'row' && it.indent;
  };
  assert.equal(ind(3), true);
  assert.equal(ind(4), true);
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

test("a note keeps its own thinking: never hidden by the layout, folded by the row's own toggle", () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'note', thinking: 'why', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'assistant', text: 'answer', thinking: 'wrap up', timestamp: 3000 }),
  ];
  // Nothing carries a hidden-thinking mark any more: the thinking sits with the note, reachable,
  // and only the thinking that has no text of its own joins a group.
  assert.deepEqual(shape(layout(rows, false)), [1, 2, 'fold[3]', 4]);
  // A turn that only thought and answered has nothing to fold: the thinking stays where it is.
  assert.deepEqual(shape(layout([rows[0]!, rows[3]!], false)), [1, 4]);
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
  // The cards stay in the flow; the plain tool runs fold into two groups.
  assert.deepEqual(shape(items), [1, 'fold[2,3]', 4, 5, 'fold[6,7]', 8]);
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

test('a page read across a roll-over stack gets one divider naming the older session', () => {
  const stack = [
    row({ id: 1, role: 'user', chatId: 'c1', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'done', chatId: 'c1', timestamp: 1000 }),
    row({ id: 3, role: 'user', text: 'carry on', chatId: 'c2', timestamp: 2000 }),
  ];
  const items = layout(stack, false, new Set(), new Map([['c1', { title: 'First turn', rolledOverAt: 1_500 }]]));
  const div = items.find((i) => i.kind === 'divider');
  assert.equal(div?.kind === 'divider' && div.sessionId === 'c1' && div.title === 'First turn' && div.rolledOverAt === 1_500, true);
  // The divider sits where the older session's rows end, before the newer turn.
  const at = items.findIndex((i) => i.kind === 'divider');
  const above = items[at - 1];
  const below = items[at + 1];
  assert.ok(above?.kind === 'row' && above.row.id === 2);
  assert.ok(below?.kind === 'row' && below.row.id === 3);
  assert.equal(estimateItem(div) > 0, true);
  // One session, or a boundary nobody told us about: still a divider (unnamed) or none at all.
  assert.equal(layout(turn(), false).some((i) => i.kind === 'divider'), false);
  assert.equal(layout(stack, false).some((i) => i.kind === 'divider'), true, 'a session change alone draws it, unnamed');
});
