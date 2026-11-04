// Dev fake for the action, saved and search ops so `vite` in a plain browser exercises the
// action buttons, saved-for-later and search without the host. `leafFakeOps()` returns the prefixes
// and handler; fakeHost.ts registers them with its `registerFakeOps` seam.
// (fakeHost.ts imports this file, so this file only imports types from it.)

import { ApiError } from '../api/client.ts';
import type { ActionButton, ActionSet } from '../api/actions.ts';
import type { SearchHit } from '../api/search.ts';
import type { FakeOpHandler } from './fakeHost.ts';

export const FAKE_SEED_BUTTONS: ActionButton[] = [
  ['update', 'Update', 'Please give me a brief plain-language status: what is finished, what is under way, what is stuck, and what you need from me.', 'topbar', false, 'prompt'],
  ['needs-me', 'Needs me', 'What needs my attention right now, and why? Only list things that truly need a decision from me (check each one) and give me the options for each.', 'topbar', false, 'prompt'],
  ['ship-it', 'Ship it', 'I am happy with this. Commit only your own changes, push, open or update the pull request, and merge to main once review passes. Tell me what landed.', 'topbar', false, 'prompt'],
  ['close-session', 'Close session', 'Please wrap up this session: record what we learned, clean up worktrees and branches, then commit and push. Report each step as pass or fail.', 'topbar', false, 'prompt'],
  ['new-session', 'New session', "Let's start a new session.", 'topbar', true, 'new_chat'],
  ['git-sync', 'Git sync', 'Please sync this repo: fetch, fast-forward only if that is safe, and tell me about anything unexpected. Do not commit.', 'overflow', false, 'prompt'],
].map(([seed, label, text, placement, hidden, kind]) => ({
  id: `seed-${seed as string}`,
  label: label as string,
  kind: kind as ActionButton['kind'],
  text: text as string,
  projectId: null,
  placement: placement as ActionButton['placement'],
  hidden: hidden as boolean,
  seedId: seed as string,
}));

export function leafFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  let set: ActionSet = { revision: 1, seedVersion: 1, buttons: FAKE_SEED_BUTTONS };
  const items = new Map<string, FakeSavedItem>();
  const notes = new Map<string, FakeSavedNote[]>();
  const now = Date.now();
  seedSaved(items, notes, now);
  const handler: FakeOpHandler = (op, args, ctx) => {
    if (op === 'actions_get') return set;
    if (op === 'actions_save') {
      if (args.revision !== set.revision) throw new ApiError('conflict', 'The buttons changed since you opened Settings.');
      set = { revision: set.revision + 1, seedVersion: set.seedVersion, buttons: args.buttons as ActionButton[] };
      return set;
    }
    if (op === 'chat_search') return fakeSearch(String(args.query ?? ''), ctx.call);
    if (op === 'saved_save') {
      const id = crypto.randomUUID();
      const at = Date.now();
      const item: FakeSavedItem = {
        id,
        title: String(args.title ?? ''),
        body: String(args.body ?? ''),
        state: 'saved',
        chatId: args.chatId == null ? null : String(args.chatId),
        messageId: args.messageId == null ? null : Number(args.messageId),
        captureKey: args.captureKey == null ? null : String(args.captureKey),
        createdAt: at,
        updatedAt: at,
        revision: 1,
        remindAt: args.remindAt == null ? null : Number(args.remindAt),
        remindedAt: null,
        noteCount: 0,
      };
      items.set(id, item);
      notes.set(id, []);
      return { item, already: false };
    }
    if (op === 'saved_list') return fakeSavedList(items, args);
    if (op === 'saved_due') {
      const now = Date.now();
      return {
        items: [...items.values()].filter(
          (i) => i.remindAt !== null && i.remindAt <= now && i.remindedAt === null && (i.state === 'saved' || i.state === 'filed'),
        ),
      };
    }
    if (op === 'saved_update') {
      const it = items.get(String(args.id));
      if (!it) throw new ApiError('not_found', 'No saved item with that id.');
      if (args.remove === true) {
        items.delete(it.id);
        notes.delete(it.id);
        return { id: it.id, removed: true };
      }
      if (typeof args.title === 'string') it.title = args.title;
      if (typeof args.body === 'string') it.body = args.body;
      if (typeof args.state === 'string') it.state = args.state as FakeSavedItem['state'];
      if ('remindAt' in args) {
        it.remindAt = args.remindAt == null ? null : Number(args.remindAt);
        it.remindedAt = null;
      }
      it.revision += 1;
      it.updatedAt = Date.now();
      return it;
    }
    if (op === 'saved_note') {
      const it = items.get(String(args.id));
      if (!it) throw new ApiError('not_found', 'No saved item with that id.');
      const note: FakeSavedNote = {
        id: (notes.get(it.id)?.length ?? 0) + 1,
        itemId: it.id,
        authorType: String(args.authorType ?? 'user'),
        authorId: null,
        text: String(args.text ?? ''),
        createdAt: Date.now(),
      };
      const list = notes.get(it.id) ?? [];
      list.push(note);
      notes.set(it.id, list);
      it.noteCount = list.length;
      it.updatedAt = note.createdAt;
      return note;
    }
    if (op === 'saved_comments') {
      if (!items.has(String(args.id))) throw new ApiError('not_found', 'No saved item with that id.');
      return { notes: notes.get(String(args.id)) ?? [] };
    }
    throw new ApiError('refused', `fake-leaf has no op ${op}`);
  };
  return { prefixes: ['actions_', 'chat_search', 'saved_'], handler };
}

interface FakeSavedItem {
  id: string;
  title: string;
  body: string;
  state: 'saved' | 'filed' | 'done' | 'dismissed';
  chatId: string | null;
  messageId: number | null;
  captureKey: string | null;
  createdAt: number;
  updatedAt: number;
  revision: number;
  remindAt: number | null;
  remindedAt: number | null;
  noteCount: number;
}
interface FakeSavedNote {
  id: number;
  itemId: string;
  authorType: string;
  authorId: string | null;
  text: string;
  createdAt: number;
}

function seedSaved(items: Map<string, FakeSavedItem>, notes: Map<string, FakeSavedNote[]>, now: number) {
  const a: FakeSavedItem = {
    id: 'fake-saved-1',
    title: 'Reword the sign-in copy',
    body: 'The wording is stiff; try something plainer.',
    state: 'saved',
    chatId: null,
    messageId: null,
    captureKey: null,
    createdAt: now - 86_400_000,
    updatedAt: now - 86_400_000,
    revision: 1,
    remindAt: now + 3_600_000,
    remindedAt: null,
    noteCount: 0,
  };
  const b: FakeSavedItem = {
    id: 'fake-saved-2',
    title: 'Ask about the nightly build',
    body: 'Did the last run finish? Check before the standup.',
    state: 'saved',
    chatId: null,
    messageId: null,
    captureKey: null,
    createdAt: now - 7_200_000,
    updatedAt: now - 60_000,
    revision: 2,
    remindAt: null,
    remindedAt: null,
    noteCount: 1,
  };
  items.set(a.id, a);
  items.set(b.id, b);
  notes.set(a.id, []);
  notes.set(b.id, [{ id: 1, itemId: b.id, authorType: 'user', authorId: null, text: 'Bring it up early.', createdAt: now - 60_000 }]);
}

function fakeSavedList(items: Map<string, FakeSavedItem>, args: Record<string, unknown>) {
  let all = [...items.values()];
  if (typeof args.state === 'string') all = all.filter((i) => i.state === args.state);
  const q = typeof args.query === 'string' ? args.query.trim().toLowerCase() : '';
  if (q) all = all.filter((i) => i.title.toLowerCase().includes(q) || i.body.toLowerCase().includes(q));
  all.sort((x, y) => y.updatedAt - x.updatedAt);
  const page = Math.max(1, Number(args.page ?? 1));
  const pageSize = 50;
  return { items: all.slice((page - 1) * pageSize, page * pageSize), total: all.length, page, pageSize };
}

interface FakeChatLike {
  id: string;
  title: string;
  cwd: string;
}
interface FakeRowLike {
  id: number;
  role: string;
  text: string;
}

/** A plain substring search over the fake host's chats, enough to see the dialog work. */
function fakeSearch(query: string, call: (op: string, args?: Record<string, unknown>) => unknown): { hits: SearchHit[] } {
  const q = query.trim().toLowerCase();
  if (!q) return { hits: [] };
  const chats = call('chat_list', {}) as FakeChatLike[];
  const titleHits: SearchHit[] = [];
  const textHits: SearchHit[] = [];
  for (const c of chats) {
    const page = call('transcript_page', { chatId: c.id }) as { messages: FakeRowLike[] };
    const row = page.messages.find((r) => (r.role === 'user' || r.role === 'assistant') && r.text.toLowerCase().includes(q));
    const project = c.cwd.split(/[\/]/).filter(Boolean).at(-1) ?? c.cwd;
    const hit: SearchHit = {
      chatId: c.id,
      title: c.title,
      project,
      messageId: row ? String(row.id) : null,
      snippet: row ? row.text.slice(0, 160) : '',
      titleOnly: !row,
    };
    if (c.title.toLowerCase().includes(q)) titleHits.push(hit);
    else if (row) textHits.push(hit);
  }
  return { hits: [...titleHits, ...textHits] };
}
