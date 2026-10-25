// The dev fake host's plan-usage ops (the top-bar meter and Settings → Usage in a plain browser).
// fakeHost.ts registers it through `registerFakeOps`. `?usage=` in the page URL picks the state:
// `live` (default), `stale`, `none` (nothing read yet), `unsupported` (the binary did not answer)
// or `chat` (only a chat's rate-limit line has reported).
//
// Dev builds only. fakeHost.ts imports this file, so this file only imports types from it.

import { ApiError } from '../api/client.ts';
import type { FeedbackEntryRow, FeedbackItemRow, FeedbackStatus, FeedbackThread } from '../api/feedback.ts';
import type { RequestModes, UsageSnapshot } from '../api/usage.ts';
import type { FakeOpHandler } from './fakeHost.ts';

type Mode = 'live' | 'stale' | 'none' | 'unsupported' | 'chat';

function modeOf(): Mode {
  const v = typeof location === 'undefined' ? '' : new URLSearchParams(location.search).get('usage');
  return v === 'stale' || v === 'none' || v === 'unsupported' || v === 'chat' ? v : 'live';
}

const HOUR = 3_600_000;

export function usageFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const mode = modeOf();
  const t0 = Date.now();
  const snapshot = (): UsageSnapshot => {
    const now = Date.now();
    const at = mode === 'stale' ? now - 40 * 60_000 : mode === 'chat' ? now - 2 * 60_000 : now - 30_000;
    const source = mode === 'chat' ? ('rate_limit_event' as const) : ('control' as const);
    if (mode === 'none') {
      return { readiness: { state: 'inactive', reason: 'No plan usage has been read yet. Try Refresh', actionLabel: 'Open Settings → Usage', actionTarget: 'settings:usage' }, refreshing: false };
    }
    if (mode === 'unsupported') {
      return {
        readiness: { state: 'inactive', reason: 'This Claude Code version did not report plan usage. Update Claude Code, or start a chat: its rate-limit messages fill the meter (Claude Code 1.0.0)', actionLabel: 'Open Settings → Usage', actionTarget: 'settings:usage', binaryVersion: '1.0.0' },
        refreshing: false,
      };
    }
    return {
      reading: {
        plan: 'max',
        fiveHour: { pct: 5, resetsAt: t0 + 4 * HOUR, at, source },
        weekly: { pct: 56, resetsAt: t0 + 50 * HOUR, at, source },
        perModel: mode === 'chat' ? [] : [{ model: 'Fable', pct: 0, resetsAt: t0 + 50 * HOUR, at, source }],
        source,
        fetchedAt: at,
        stale: mode === 'stale',
      },
      readiness:
        mode === 'chat'
          ? { state: 'ready', reason: 'This Claude Code version did not report plan usage; showing the last reading from a chat (Claude Code 1.0.0)', binaryVersion: '1.0.0' }
          : { state: 'ready', reason: 'Plan usage is up to date.' },
      refreshing: false,
    };
  };
  const modes: RequestModes = { fast: 12, standard: 340, unknown: 3, files: 4, malformed: 0, oversized: 0 };
  // Feedback items: a few threads in memory, so the tab can be tried in a browser.
  const threads: FeedbackThread[] = [
    {
      id: 2, title: 'Search takes three seconds on a big repo', status: 'open', boardItem: null, author: { kind: 'agent', chatId: 'c-fake-1' }, createdAt: t0 - 3 * HOUR, updatedAt: t0 - HOUR,
      entries: [
        { id: 3, text: 'quick_open was slow in a repo with 40k files.', author: { kind: 'agent', chatId: 'c-fake-1' }, at: t0 - 3 * HOUR },
        { id: 4, text: 'Same here. <b>plain text only</b>', author: { kind: 'human' }, at: t0 - HOUR },
      ],
    },
    {
      id: 1, title: 'Sidebar jumps when the window is resized', status: 'filed', boardItem: '6c91e818-ef71', author: { kind: 'human' }, createdAt: t0 - 30 * HOUR, updatedAt: t0 - 30 * HOUR,
      entries: [{ id: 1, text: 'It jumps back to the top.', author: { kind: 'human' }, at: t0 - 30 * HOUR }],
    },
    {
      id: 3, title: 'Wants a darker map view', status: 'open', boardItem: null, author: { kind: 'agent_for_user', chatId: 'c-fake-2' }, createdAt: t0 - 5 * HOUR, updatedAt: t0 - 5 * HOUR,
      entries: [{ id: 2, text: 'The user said the map is too bright at night.', author: { kind: 'agent_for_user', chatId: 'c-fake-2' }, at: t0 - 5 * HOUR }],
    },
  ];
  let nextEntry = 10;
  const row = (t: FeedbackThread): FeedbackItemRow => ({ id: t.id, title: t.title, status: t.status, boardItem: t.boardItem, author: t.author, createdAt: t.createdAt, updatedAt: t.updatedAt, entryCount: t.entries.length, snippet: t.entries[0]?.text ?? '' });
  const thread = (id: unknown) => {
    const t = threads.find((x) => x.id === id);
    if (!t) throw new ApiError('bad_args', `There is no feedback item ${String(id)}.`);
    return t;
  };
  const feedbackOp = (op: string, a: Record<string, unknown>): unknown => {
    const now = Date.now();
    switch (op) {
      case 'feedback_items': {
        const text = typeof a.text === 'string' ? a.text.toLowerCase() : '';
        const hits = threads.filter((t) => (!a.status || t.status === a.status) && (!a.author || t.author.kind === a.author) && (!text || t.title.toLowerCase().includes(text) || t.entries.some((e) => e.text.toLowerCase().includes(text))));
        return { items: hits.sort((x, y) => y.updatedAt - x.updatedAt).map(row) };
      }
      case 'feedback_item':
        return thread(a.id);
      case 'feedback_create': {
        const title = String(a.title ?? '').trim();
        const text = String(a.text ?? '').trim();
        if (!title || !text) throw new ApiError('bad_args', 'A feedback item needs a title and some text.');
        const id = Math.max(0, ...threads.map((t) => t.id)) + 1;
        threads.push({ id, title, status: 'open', boardItem: null, author: { kind: 'human' }, createdAt: now, updatedAt: now, entries: [{ id: nextEntry++, text, author: { kind: 'human' }, at: now }] });
        return { id };
      }
      case 'feedback_append': {
        const t = thread(a.id);
        const text = String(a.text ?? '').trim();
        if (!text) throw new ApiError('bad_args', 'The text cannot be empty.');
        const e: FeedbackEntryRow = { id: nextEntry++, text, author: { kind: 'human' }, at: now };
        t.entries.push(e);
        t.updatedAt = now;
        return { id: t.id };
      }
      default: {
        const t = thread(a.id);
        if (typeof a.status === 'string') t.status = a.status as FeedbackStatus;
        if (typeof a.boardItem === 'string') {
          t.boardItem = a.boardItem;
          if (t.status === 'open' && typeof a.status !== 'string') t.status = 'filed';
        }
        t.updatedAt = now;
        return { id: t.id };
      }
    }
  };
  const handler: FakeOpHandler = (op, a, ctx) => {
    switch (op) {
      case 'feedback_items':
      case 'feedback_item':
      case 'feedback_create':
      case 'feedback_append':
      case 'feedback_update':
        return feedbackOp(op, a);
      case 'usage_get':
        return snapshot();
      case 'usage_refresh':
        setTimeout(() => ctx.emit({ kind: 'plan_usage', ...snapshot() }), 50);
        return { ...snapshot(), refreshing: true };
      case 'request_modes': {
        if (typeof a.since !== 'number' || typeof a.until !== 'number' || a.until <= a.since) throw new ApiError('bad_args', 'The range must end after it starts.');
        return a.since === 0 ? { ...modes, fast: 40, standard: 2_100 } : modes;
      }
      case 'feedback_report':
        return {
          features: [
            { feature: 'app_list_chats', count: 6, mean: 1.5, histogram: { '-2': 0, '-1': 0, '0': 1, '1': 2, '2': 3 }, kinds: ['existing'], newestNotes: ['Found the chat I needed at once.', '<b>not markup</b> & plain text only'] },
            { feature: 'Settings undo', count: 2, mean: -1, histogram: { '-2': 1, '-1': 1, '0': 0, '1': 0, '2': 0 }, kinds: ['tweak', 'missing'], newestNotes: ['The toast vanished before I could read it.'] },
          ],
        };
      case 'feedback_export':
        return { entries: [{ chatId: 'c1', feature: 'app_list_chats', score: 2, kind: 'existing', note: 'ok', context: null, origin: 'agent', at: t0 }] };
      default:
        throw new ApiError('refused', 'This operation is not available here.');
    }
  };
  return { prefixes: ['usage_', 'request_modes', 'feedback_'], handler };
}
