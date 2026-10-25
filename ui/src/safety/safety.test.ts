import { test } from 'node:test';
import assert from 'node:assert/strict';
import { installContextMenuPolicy, nativeMenuWanted } from './contextMenu.ts';
import { describe, ErrorReporter, installErrorReporting, MAX_PER_PAGE, REPEAT_MS } from './report.ts';

const el = (matchesEditable: boolean) => ({ closest: () => (matchesEditable ? {} : null) }) as unknown as Element;

test('right-click keeps the native menu only in fields, over a selection, or in a dev run', () => {
  const rows: [Element | null, string, boolean, boolean][] = [
    [el(false), '', false, false], // plain page: Helm's own menu or nothing
    [null, '', false, false],
    [el(true), '', false, true], // a text field or the editor
    [el(false), 'some text', false, true], // selected text can be copied
    [el(false), '', true, true], // development keeps Inspect
  ];
  for (const [target, sel, dev, want] of rows) assert.equal(nativeMenuWanted(target, sel, dev), want, `${sel} dev=${dev}`);

  // The listener suppresses the menu unless Helm already handled it or the policy keeps it.
  let handler: ((e: unknown) => void) | undefined;
  const doc = {
    addEventListener: (_: string, h: (e: unknown) => void) => (handler = h),
    removeEventListener: () => undefined,
    getSelection: () => ({ toString: () => '' }),
  } as unknown as Document;
  installContextMenuPolicy(doc, false);
  const fire = (defaultPrevented: boolean) => {
    let prevented = false;
    handler?.({ defaultPrevented, target: undefined, preventDefault: () => (prevented = true) });
    return prevented;
  };
  assert.equal(fire(false), true);
  assert.equal(fire(true), false);
});

test('error reports: described, capped per page, repeats skipped, window errors forwarded', () => {
  assert.deepEqual(describe('plain'), { message: 'plain', stack: '' });
  assert.deepEqual(describe({ a: 1 }), { message: '{"a":1}', stack: '' });
  const err = new TypeError('bad');
  assert.equal(describe(err).message, 'TypeError: bad');

  let t = 0;
  const sent: Record<string, string>[] = [];
  const r = new ErrorReporter((a) => (sent.push(a), Promise.reject(new Error('host gone'))), () => t);
  assert.equal(r.report('error', err, 'a.js:1:1'), true);
  assert.equal(r.report('error', err), false, 'the same message right away');
  t += REPEAT_MS;
  assert.equal(r.report('error', err), true, 'later again');
  assert.equal(r.report('pane', err), true, 'another kind');
  for (let i = 0; sent.length < MAX_PER_PAGE; i++) r.report('error', `n${i}`);
  assert.equal(r.report('error', 'one more'), false, 'the per-page cap');
  assert.deepEqual(sent[0], { kind: 'error', message: 'TypeError: bad', stack: err.stack ?? '', source: 'a.js:1:1' });

  const listeners = new Map<string, (e: Event) => void>();
  const calls: Record<string, unknown>[] = [];
  const off = installErrorReporting(
    (async (op: string, args: Record<string, unknown>) => (calls.push({ op, ...args }), {})) as never,
    { addEventListener: (n: string, h: (e: Event) => void) => listeners.set(n, h), removeEventListener: () => undefined } as never,
  );
  listeners.get('error')?.({ error: new Error('boom'), message: 'boom', filename: 'x.js', lineno: 3, colno: 4 } as unknown as Event);
  listeners.get('unhandledrejection')?.({ reason: 'nope' } as unknown as Event);
  assert.deepEqual(calls.map((c) => [c.op, c.kind, c.source]), [['ui_error_report', 'error', 'x.js:3:4'], ['ui_error_report', 'rejection', '']]);
  off();
});
