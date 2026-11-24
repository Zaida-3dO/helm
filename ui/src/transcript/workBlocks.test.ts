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

test('a settled turn folds its whole run of work into ONE group sitting under its messages; a failed call folds like a passed one', () => {
  const items = layout(turn(), false);
  // The notes and the reply stand in the flow, in order; every tool call folds into the one group.
  assert.deepEqual(shape(items), [1, 2, 5, 'fold[3,4]']);
  const fold = items.find((i) => i.kind === 'fold');
  // Timed over the group itself: the two calls at 2 s and 3 s.
  assert.equal(fold?.kind === 'fold' && fold.elapsedMs, 1000);
  assert.equal(fold?.kind === 'fold' && fold.failed, 1);
  assert.equal(fold?.kind === 'fold' && fold.tools, 2);
});

test('a turn with no tool call and no thinking has no group at all, however long it took to start', () => {
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
  assert.deepEqual(shape(items), [1, 2, 5, 'fold[3,4]']);
  assert.equal(items.find((i) => i.kind === 'row' && i.row.id === 5 && i.final) !== undefined, true);
});

test('thinking alone is work: the turn folds it, reachable, not hidden', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', thinking: 'weighing it up', timestamp: 30_000 }),
    row({ id: 3, role: 'assistant', text: 'answer', timestamp: 42_000 }),
  ];
  const items = layout(rows, false);
  assert.deepEqual(shape(items), [1, 3, 'fold[2]']);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.thoughts, 1);
  // Opened, the thinking row itself appears, under the group's rule.
  const opened = layout(rows, false, new Set([revealKey('k1')]));
  assert.deepEqual(shape(opened), [1, 3, 'fold[2]', 2]);
  const thought = opened.find((i) => i.kind === 'row' && i.row.id === 2);
  assert.ok(thought?.kind === 'row' && thought.indent, 'the opened group lists its rows, indented');
});

test('the work group sits after the reply, which keeps its hairline; opening the group lists its rows under the header', () => {
  const items = layout(turn(), false);
  const foldAt = items.findIndex((i) => i.kind === 'fold');
  const replyAt = items.findIndex((i) => i.kind === 'row' && i.row.id === 5);
  assert.ok(foldAt > replyAt, 'the group is the last item of the turn, under the final reply');
  const reply = items[replyAt]!;
  assert.equal(reply?.kind === 'row' && reply.final, true);
  assert.equal(reply?.kind === 'row' && reply.afterFold, false, 'nothing sits between the reply and its hairline');
  const open = layout(turn(), false, new Set([revealKey('k1')]));
  assert.deepEqual(shape(open), [1, 2, 5, 'fold[3,4]', 3, 4]);
  const openedReply = open.find((i) => i.kind === 'row' && i.row.id === 5);
  assert.equal(openedReply?.kind === 'row' && openedReply.final, true, 'the reply keeps its final look with the group opened');
});

test('a live turn folds the same single group as the settled one; only the header line changes', () => {
  const live = layout(turn(), true);
  const settled = layout(turn(), false);
  assert.deepEqual(shape(live), [1, 2, 5, 'fold[3,4]']);
  assert.deepEqual(shape(live), shape(settled));
  // The reply being written after the last tool already has its final look.
  const flags = (items: Item[]) => items.map((i) => (i.kind === 'row' ? [i.final, i.afterFold, i.afterTool] : 'fold'));
  assert.deepEqual(flags(live), flags(settled));
  // Live, the group carries the working line; settled it does not.
  assert.equal(live.find((i) => i.kind === 'fold')?.kind === 'fold' && (live.find((i) => i.kind === 'fold') as { live: boolean }).live, true);
  assert.equal(settled.find((i) => i.kind === 'fold')?.kind === 'fold' && (settled.find((i) => i.kind === 'fold') as { live: boolean }).live, false);
});

test('while a tool is the newest row, it folds into the bottom group and the header names the running call', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'let me look', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'assistant', text: 'now the tests', timestamp: 3000 }),
    row({ id: 5, role: 'toolResult', toolName: 'Bash', status: 'streaming', timestamp: 4000 }),
  ];
  const live = layout(rows, true);
  assert.deepEqual(shape(live), [1, 2, 4, 'fold[3,5]']);
  const fold = live.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.live && fold.working, 'Bash', 'the header names the running call');
  // Text written after it joins the flow; the finished call sits in the same group.
  const more = [...rows.slice(0, 4), { ...rows[4]!, status: 'complete' as const }, row({ id: 6, role: 'assistant', text: 'All', timestamp: 5000 })];
  assert.deepEqual(shape(layout(more, true)), [1, 2, 4, 6, 'fold[3,5]']);
});

test('live, a single running call folds under a header that says what it is doing', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    // The empty message the host saves a tool call under.
    row({ id: 2, role: 'assistant', text: '', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Bash', status: 'streaming', details: { toolSummary: 'building the app' }, timestamp: 1000 }),
  ];
  const live = layout(rows, true);
  assert.deepEqual(shape(live), [1, 'fold[2,3]']);
  assert.equal(live.find((i) => i.kind === 'fold')?.kind === 'fold' && (live.find((i) => i.kind === 'fold') as { working: string }).working, 'building the app');
  const second = [...rows, row({ id: 4, role: 'assistant', text: '', timestamp: 2000 }), row({ id: 5, role: 'toolResult', toolName: 'Bash', status: 'streaming', timestamp: 2000 })];
  assert.deepEqual(shape(layout(second, true)), [1, 'fold[2,3,4,5]']);
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
  // Closed: both calls fold into the one group under the note and the reply; the counts are
  // the calls themselves, not each call's empty message.
  const closed = layout(rows, false);
  assert.deepEqual(shape(closed), [1, 4, 7, 'fold[2,3,5,6]']);
  const folds = closed.filter((i) => i.kind === 'fold');
  assert.deepEqual(folds.map((f) => (f.kind === 'fold' ? f.tools : 0)), [2]);
  // Opened: the calls are listed after the header, the empty messages take no room between them.
  const opened = layout(rows, false, new Set([revealKey('k1')]));
  assert.deepEqual(shape(opened), [1, 4, 7, 'fold[2,3,5,6]', 3, 6]);
  assert.deepEqual(
    opened.flatMap((i) => (i.kind === 'row' && i.indent ? [i.row.id] : [])),
    [3, 6],
  );
  // Opening the turn opens its one group.
  assert.deepEqual(shape(layout(rows, false, new Set(['cg:k1:2']))), shape(opened));
  // Live, the running call folds under the working header, empty message and all.
  const live = [
    rows[0]!,
    { ...rows[1]!, status: 'streaming' as const },
    { ...rows[2]!, status: 'streaming' as const },
  ];
  assert.deepEqual(shape(layout(live, true)), [1, 'fold[2,3]']);
  // A turn whose whole reply is empty messages shows the prompt and draws nothing else.
  assert.deepEqual(shape(layout([row({ id: 1, role: 'user', timestamp: 0 }), row({ id: 2, role: 'assistant', text: '', thinking: '  ', timestamp: 1000 })], false)), [1]);
});

test('a live turn that only thought folds its thinking behind the working header', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: '', thinking: 'hmm', timestamp: 1000 }),
    row({ id: 3, role: 'assistant', text: 'Here', timestamp: 2000 }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 3, 'fold[2]']);
  assert.deepEqual(shape(layout(rows, false)), [1, 3, 'fold[2]']);
  assert.equal(layout(rows, true).find((i) => i.kind === 'fold')?.kind === 'fold' && (layout(rows, true).find((i) => i.kind === 'fold') as { working: string }).working, 'Thinking');
});

test('Agent calls stay in the flow, side by side as one group, and the folded call sits below them all', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'toolResult', toolName: 'Agent', toolCallId: 'a1', timestamp: 1000 }),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'toolResult', toolName: 'Task', toolCallId: 'a2', timestamp: 3000 }),
    row({ id: 5, role: 'assistant', text: 'Started two reviewers.', timestamp: 4000 }),
    row({ id: 6, role: 'toolResult', toolName: 'Agent', toolCallId: 'a3', timestamp: 5000 }),
  ];
  // The cards stay in the flow as they arrived; the plain call folds into the turn's group.
  assert.deepEqual(shape(layout(rows, false)), [1, 'agents[2,4]', 5, 'agents[6]', 'fold[3]']);
  assert.deepEqual(shape(layout(rows, true)), shape(layout(rows, false)));
  // Opened, the Read appears under the group header, after the cards.
  assert.deepEqual(shape(layout(rows, false, new Set([revealKey('k1')]))), [1, 'agents[2,4]', 5, 'agents[6]', 'fold[3]', 3]);
});

test('an earlier turn folds while the latest is live; opening the turn unfolds its work', () => {
  const rows = [
    ...turn(),
    row({ id: 6, role: 'user', timestamp: 70_000 }),
    row({ id: 7, role: 'toolResult', timestamp: 71_000 }),
  ];
  assert.deepEqual(shape(layout(rows, true)), [1, 2, 5, 'fold[3,4]', 6, 'fold[7]']);
  // Opened: the group row stays (so it can be closed again) and the work follows in order.
  assert.deepEqual(shape(layout(rows, true, new Set([revealKey('k1')]))), [1, 2, 5, 'fold[3,4]', 3, 4, 6, 'fold[7]']);
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
  assert.deepEqual(shape(layout(rows, false)), [1, 2, 3, 8, 5, 'fold[4]']);
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

test('foldLabel says what the group holds, how long it took, and live what it is doing now', () => {
  assert.equal(foldLabel({ tools: 12, thoughts: 0, messages: 0, elapsedMs: 40_000 }), '12 tool calls · worked for 40s');
  assert.equal(foldLabel({ tools: 1, thoughts: 0, messages: 0, elapsedMs: 1000 }), '1 tool call · worked for 1s');
  assert.equal(foldLabel({ tools: 0, thoughts: 2, messages: 1, elapsedMs: 0 }), '2 thoughts · 1 message');
  assert.equal(foldLabel({ tools: 3, thoughts: 1, messages: 2, elapsedMs: 65_000 }), '3 tool calls · 1 thought · 2 messages · worked for 1m 5s');
  assert.equal(foldLabel({ tools: 4, thoughts: 1, messages: 0, elapsedMs: 20_000, working: 'reading the config' }), 'reading the config · 4 tool calls · 1 thought · worked for 20s');
});

test('a steer read mid-turn stays in the turn and is never folded, live or settled', () => {
  const rows = [
    row({ id: 1, role: 'user' }),
    row({ id: 2, role: 'toolResult' }),
    row({ id: 3, role: 'user', text: 'also this', details: { mode: 'steer' } }),
    row({ id: 4, role: 'toolResult' }),
    row({ id: 5, role: 'assistant', text: 'reply' }),
  ];
  // The steer is the person's own words: it stands in the flow, and both calls still fold into
  // the turn's one group underneath.
  assert.deepEqual(shape(layout(rows, true)), [1, 3, 5, 'fold[2,4]']);
  assert.deepEqual(shape(layout(rows, false)), [1, 3, 5, 'fold[2,4]']);
});

test('a message from another chat, a schedule or the app mid-turn neither opens a turn nor stays in the flow: it folds into the turn\'s group', () => {
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
  // Two calls and both cards make the one group: the message cards count as steps toward the label.
  assert.deepEqual(shape(items), [1, 6, 'fold[2,3,4,5]']);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.tools, 2);
  assert.equal(fold?.kind === 'fold' && fold.messages, 2);
  // Opened, the cards appear among the calls, each one visible.
  const opened = layout(rows, false, new Set([revealKey('k1')]));
  assert.deepEqual(shape(opened), [1, 6, 'fold[2,3,4,5]', 2, 3, 4, 5]);
  assert.deepEqual(
    opened.flatMap((i) => (i.kind === 'row' && i.indent ? [i.row.id] : [])),
    [2, 3, 4, 5],
  );
});

test('a prompt-mode message from another chat mid-turn keeps the current turn going', () => {
  const rows = [
    row({ id: 1, role: 'user', text: 'build it', timestamp: 0 }),
    row({ id: 2, role: 'toolResult', timestamp: 1000 }),
    row({
      id: 3,
      role: 'user',
      text: '[Message from another chat: "Coordinator" (chat-9)]\nSent by another Helm chat.\n\nCarry on.',
      details: { origin: 'other_chat' },
      timestamp: 2000,
    }),
    row({ id: 4, role: 'toolResult', timestamp: 3000 }),
    row({ id: 5, role: 'assistant', text: 'reply', timestamp: 4000 }),
  ];
  // The other chat's message is agent traffic, not the person speaking: one turn, one group.
  assert.deepEqual(shape(layout(rows, false)), [1, 5, 'fold[2,3,4]']);
  // Only the person's own send opens a new turn.
  const typed = [...rows, row({ id: 6, role: 'user', text: 'and now this', timestamp: 5000 }), row({ id: 7, role: 'assistant', text: 'ok', timestamp: 6000 })];
  assert.deepEqual(shape(layout(typed, false)), [1, 5, 'fold[2,3,4]', 6, 7]);
});

test('a relayed user action stays in the flow, not folded with the work', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'toolResult', timestamp: 1000 }),
    row({
      id: 3,
      role: 'user',
      text: '[Message from another chat: "Gate" (chat-4)]\nRelayed.\n\nThe user clicked Continue.',
      details: { origin: 'other_chat', kind: 'user-action' },
      timestamp: 2000,
    }),
    row({ id: 4, role: 'toolResult', timestamp: 3000 }),
    row({ id: 5, role: 'assistant', text: 'reply', timestamp: 4000 }),
  ];
  assert.deepEqual(shape(layout(rows, false)), [1, 3, 5, 'fold[2,4]']);
});

test('declared progress lines: the newest stands under the group, superseded ones fold, and a mid-turn message keeps the line current when it was sent above it', () => {
  const prog = (id: number, text: string, ts: number) =>
    row({ id, role: 'toolResult', toolName: 'mcp__helm__app_progress', details: { toolArgs: JSON.stringify({ text }) }, timestamp: ts });
  // Live: the newest line is the turn's last item, under the group; the older one folds with the calls.
  const base = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    prog(2, 'Reading the code', 1000),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    prog(4, 'Running the tests', 3000),
    row({ id: 5, role: 'toolResult', toolName: 'Bash', status: 'streaming', timestamp: 4000 }),
  ];
  const live = layout(base, true);
  assert.deepEqual(shape(live), [1, 'fold[2,3,5]', 'progress']);
  const line = live[live.length - 1]!;
  assert.ok(line.kind === 'progress' && line.text === 'Running the tests' && line.pinned === undefined, 'the newest declared line, by its text');
  // A turn whose only work is a declared line has no group at all: just the line under the prompt.
  assert.deepEqual(shape(layout([row({ id: 1, role: 'user', timestamp: 0 }), prog(2, 'Starting', 1000)], true)), [1, 'progress']);
  // A real reply or a note after it folds the line away: the agent spoke, the heartbeat is history.
  const replied = [...base, row({ id: 6, role: 'assistant', text: 'Green.', timestamp: 5000 })];
  assert.deepEqual(shape(layout(replied, false)), [1, 6, 'fold[2,3,4,5]']);
  const noted = [...base, row({ id: 6, role: 'assistant', text: 'tests next', timestamp: 5000 })];
  assert.deepEqual(shape(layout(noted, true)), [1, 6, 'fold[2,3,4,5]']);
  // The person speaks mid-turn: the line current at that moment stays in the flow above their
  // message; the call that was running then and the later line fold under the group.
  const steered = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    prog(2, 'Reading the code', 1000),
    row({ id: 3, role: 'toolResult', toolName: 'Read', timestamp: 2000 }),
    row({ id: 4, role: 'user', text: 'also do this', details: { mode: 'steer' }, timestamp: 3000 }),
    prog(5, 'Adding it', 3500),
    row({ id: 6, role: 'assistant', text: 'done', timestamp: 4000 }),
  ];
  const st = layout(steered, false);
  assert.deepEqual(shape(st), [1, 'progress', 4, 6, 'fold[3,5]']);
  const mark = st[1]!;
  assert.ok(mark.kind === 'progress' && mark.pinned === true && mark.text === 'Reading the code', 'the line that was current when they spoke marks the turn there');
  // Opened, the folded progress rows list among the group's rows, and the live line stays last.
  const opened = layout(base, true, new Set([revealKey('k1')]));
  assert.deepEqual(shape(opened), [1, 'fold[2,3,5]', 2, 3, 5, 'progress']);
  // A failed progress call is no line at all: it folds as an ordinary tool row.
  const failed = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'toolResult', toolName: 'mcp__helm__app_progress', details: { toolArgs: '{"text":"x"}', isError: true }, timestamp: 1000 }),
    row({ id: 3, role: 'assistant', text: 'done', timestamp: 2000 }),
  ];
  assert.deepEqual(shape(layout(failed, false)), [1, 3, 'fold[2]']);
});

test('short aside notes fold into the group with their thinking; shape decides, not length', () => {
  const rows = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'Let me check the tests first, then run the build.', timestamp: 1000 }),
    row({ id: 3, role: 'assistant', text: '(Correct)', timestamp: 2000 }),
    row({ id: 4, role: 'assistant', text: '[ok]', thinking: 'the seam holds', timestamp: 3000 }),
    row({ id: 5, role: 'toolResult', timestamp: 4000 }),
    row({ id: 6, role: 'assistant', text: 'Done.', timestamp: 5000 }),
  ];
  const items = layout(rows, false);
  assert.deepEqual(shape(items), [1, 2, 6, 'fold[3,4,5]']);
  assert.equal(items.find((i) => i.kind === 'fold')?.kind === 'fold' && (items.find((i) => i.kind === 'fold') as { thoughts: number }).thoughts, 1, "the aside's thinking counts as a thought");
  // A note with no letters or digits at all folds too; and a wrapped note at the end is an aside,
  // not the final reply — the last real note keeps that role.
  const rows2 = [
    row({ id: 1, role: 'user', timestamp: 0 }),
    row({ id: 2, role: 'assistant', text: 'All set. The tests pass and the build is green.', timestamp: 1000 }),
    row({ id: 3, role: 'assistant', text: '(Done.)', timestamp: 2000 }),
  ];
  assert.deepEqual(shape(layout(rows2, false)), [1, 2, 'fold[3]']);
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
  assert.deepEqual(shape(items), [1, 5, 'fold[2,3,4]']);
  const fold = items.find((i) => i.kind === 'fold');
  assert.equal(fold?.kind === 'fold' && fold.failed, 2);
  const reply = items.find((i) => i.kind === 'row' && i.row.id === 5);
  assert.equal(reply?.kind === 'row' && reply.final, true);
  // A plain answer with no tool work has no hairline.
  const plain = layout([row({ id: 1, role: 'user' }), row({ id: 2, role: 'assistant', text: 'hi' })], false);
  const hi = plain.find((i) => i.kind === 'row' && i.row.id === 2);
  assert.equal(hi?.kind === 'row' && hi.final, false);
});

test('a long busy turn ends as three notes, the reply and ONE group holding every call', () => {
  const rows: MessageRow[] = [row({ id: 1, role: 'user', text: 'do the thing', timestamp: 0 })];
  let id = 2;
  for (let n = 0; n < 3; n++) {
    for (let c = 0; c < 22; c++) rows.push(row({ id: id++, role: 'toolResult', toolName: 'Edit', timestamp: id * 1000 }));
    rows.push(row({ id: id++, role: 'assistant', text: `note ${n + 1}`, timestamp: id * 1000 }));
  }
  rows.push(row({ id, role: 'assistant', text: 'all done', timestamp: id * 1000 }));
  const items = layout(rows, false);
  const folds = items.filter((i) => i.kind === 'fold');
  assert.equal(folds.length, 1, 'one group for the whole turn, however scattered the calls were');
  assert.equal(folds[0]!.kind === 'fold' && folds[0]!.rows.length, 66);
  // The three notes and the reply stand above it, in order, and the group is the turn's last item.
  const shown = items.filter((i) => i.kind === 'row').map((i) => (i.kind === 'row' ? i.row.text : ''));
  assert.deepEqual(shown.slice(1, 4), ['note 1', 'note 2', 'note 3']);
  assert.equal(items[items.length - 1]!.kind, 'fold');
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
  // The thinking sits with the note, reachable, and only the thinking that has no text of its
  // own joins the group.
  assert.deepEqual(shape(layout(rows, false)), [1, 2, 4, 'fold[3]']);
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
  // The cards stay in the flow; every plain tool run folds into the one group under them.
  assert.deepEqual(shape(items), [1, 4, 5, 8, 'fold[2,3,6,7]']);
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

test('turns without a typed opener, helm notes, replaced progress lines and settled progress lines', () => {
  const prog = (id: number, text: string, ts: number) =>
    row({ id, role: 'toolResult', toolName: 'mcp__helm__app_progress', details: { toolArgs: JSON.stringify({ text }) }, timestamp: ts });
  const peer = (id: number) =>
    row({ id, role: 'user', text: '[Message from another chat: "Boss" (chat-9)]\nSent by another Helm chat.\n\nGo.', details: { origin: 'other_chat', kind: 'initial-prompt' } });
  const call = (id: number, ts: number) => row({ id, role: 'toolResult', toolName: 'Read', timestamp: ts });
  const blank = (id: number, ts: number) => row({ id, role: 'assistant', text: '', timestamp: ts });
  const note = (id: number, text: string, ts: number) => row({ id, role: 'assistant', text, timestamp: ts });
  const helmNote = (id: number) => row({ id, role: 'user', text: '[Attention] something finished', details: { origin: 'app' } });
  const pageSend = (id: number) => row({ id, role: 'user', text: 'Run it', details: { origin: 'page', pageTitle: 'Board' } });
  const cases: { name: string; rows: MessageRow[]; live?: boolean; want: (string | number)[] }[] = [
    {
      name: 'a chat started by another chat has no typed message: its leading rows are one turn with one group',
      rows: [peer(1), blank(2, 1000), call(3, 2000), blank(4, 3000), call(5, 4000), note(6, 'All done.', 5000)],
      want: [6, 'fold[1,2,3,4,5]'],
    },
    {
      name: 'a page that starts mid-turn folds that turn too, and the next typed message opens the next turn',
      rows: [call(1, 0), blank(2, 1000), call(3, 2000), note(4, 'Finished.', 3000), row({ id: 5, role: 'user', text: 'thanks', timestamp: 4000 }), note(6, 'Welcome.', 5000)],
      want: [4, 'fold[1,2,3]', 5, 6],
    },
    {
      name: 'a click on a page button is the person speaking: it opens a turn',
      rows: [row({ id: 1, role: 'user', timestamp: 0 }), call(2, 1000), note(3, 'one', 2000), pageSend(4), call(5, 3000), note(6, 'two', 4000)],
      want: [1, 3, 'fold[2]', 4, 6, 'fold[5]'],
    },
    {
      name: 'a note for the agent alone is never drawn, and does not pin the progress line',
      rows: [row({ id: 1, role: 'user', timestamp: 0 }), prog(2, 'Working', 1000), helmNote(3), call(4, 2000), note(5, 'done', 3000)],
      want: [1, 5, 'fold[2,4]'],
    },
    {
      name: 'a progress line a real agent message already replaced is not pinned above a later steer',
      rows: [
        row({ id: 1, role: 'user', timestamp: 0 }),
        prog(2, 'Working', 1000),
        note(3, 'Halfway there, looking good.', 2000),
        row({ id: 4, role: 'user', text: 'and this', details: { mode: 'steer' }, timestamp: 3000 }),
        note(5, 'done', 4000),
      ],
      want: [1, 3, 4, 5, 'fold[2]'],
    },
    {
      name: 'a settled turn that ended without a reply folds its last progress line into the group',
      rows: [row({ id: 1, role: 'user', timestamp: 0 }), call(2, 1000), prog(3, 'Running the tests', 2000)],
      want: [1, 'fold[2,3]'],
    },
    {
      name: 'a running turn keeps its newest progress line under the group',
      rows: [row({ id: 1, role: 'user', timestamp: 0 }), call(2, 1000), prog(3, 'Running the tests', 2000)],
      live: true,
      want: [1, 'fold[2]', 'progress'],
    },
  ];
  for (const c of cases) assert.deepEqual(shape(layout(c.rows, c.live ?? false)), c.want, c.name);
});
