import { test } from 'node:test';
import assert from 'node:assert/strict';
import { hasWork, railMarkers, timelineEvents, visibleSpan } from './timeline.ts';
import { row } from '../dev/fixtures.ts';

const tool = (id: number, toolName: string, args: object, extra: Partial<Parameters<typeof row>[0]> = {}) =>
  row({ id, role: 'toolResult', toolName, details: { toolArgs: JSON.stringify(args) }, ...extra });

test('prompts and work become events; reads and searches do not', () => {
  const events = timelineEvents([
    row({ id: 1, role: 'user', text: 'Fix  the\nbuild' }),
    tool(2, 'Read', { file_path: 'src/a.ts' }),
    tool(3, 'Edit', { file_path: 'src/a.ts' }),
    tool(4, 'Write', { file_path: 'src/b.ts' }),
    tool(5, 'Bash', { command: 'npm test' }),
    tool(6, 'Bash', { command: 'ls -la' }),
    tool(7, 'Bash', { command: 'npm run build' }, { details: { toolArgs: '{"command":"npm run build"}', isError: true } }),
    tool(8, 'Bash', { command: 'cargo build' }, { status: 'streaming' }),
    row({ id: 9, role: 'assistant', text: 'done' }),
  ]);
  assert.deepEqual(
    events.map((e) => [e.rowId, e.kind, e.title]),
    [
      [1, 'prompt', 'Prompt'],
      [3, 'edit', 'Edited file'],
      [4, 'write', 'Wrote file'],
      [5, 'test', 'Tests passed'],
      [6, 'command', 'Ran command'],
      [7, 'failed', 'Command failed'],
      [8, 'running', 'Running command'],
    ],
  );
  assert.equal(events[0]!.detail, 'Fix the build');
});

test('the rail shows only once there is work beyond prompts', () => {
  assert.equal(hasWork(timelineEvents([row({ id: 1, role: 'user', text: 'hi' })])), false);
  assert.equal(hasWork(timelineEvents([row({ id: 1, role: 'user' }), tool(2, 'Bash', { command: 'ls' })])), true);
});

test('more events than slots cluster, keeping the most telling event of each group', () => {
  const events = Array.from({ length: 20 }, (_, i) => ({ rowId: i, kind: 'command' as const, title: 'Ran command', detail: '', body: '', diff: [], output: '' }));
  events[5] = { ...events[5]!, kind: 'failed' as never, title: 'Command failed' };
  const markers = railMarkers(events, 8 * 22);
  assert.equal(markers.length, 8);
  assert.equal(markers.reduce((n, m) => n + m.size, 0), 20);
  assert.ok(markers.some((m) => m.event.rowId === 5 && m.size > 1));
  // Fewer events than slots: one marker each.
  assert.equal(railMarkers(events.slice(0, 5), 600).every((m) => m.size === 1), true);
});

test('events carry what the preview card shows: a prompt body, an edit diff, a command output', () => {
  const events = timelineEvents([
    row({ id: 1, role: 'user', text: 'Fix  the\nbuild' }),
    tool(2, 'Edit', { file_path: 'a.ts', old_string: 'x', new_string: 'y' }),
    tool(3, 'Bash', { command: 'ls' }, { text: 'a.ts\nb.ts' }),
  ]);
  assert.equal(events[0]!.body, 'Fix  the\nbuild');
  assert.deepEqual(events[1]!.diff, [
    { sign: '-', text: 'x' },
    { sign: '+', text: 'y' },
  ]);
  assert.equal(events[1]!.output, '');
  assert.equal(events[2]!.output, 'a.ts\nb.ts');
  assert.deepEqual(events[2]!.diff, []);
});

test('the visible band spans the first to last marker whose rows are on screen', () => {
  const events = Array.from({ length: 6 }, (_, i) => ({ rowId: (i + 1) * 10, kind: 'command' as const, title: '', detail: '', body: '', diff: [], output: '' }));
  const markers = railMarkers(events, 600);
  assert.deepEqual(visibleSpan(markers, 20, 40), { first: 1, last: 3 });
  assert.deepEqual(visibleSpan(markers, 25, 25), null);
  assert.deepEqual(visibleSpan(markers, 0, 1000), { first: 0, last: 5 });
});
