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
  const handler: FakeOpHandler = (op, args, ctx) => {
    if (op === 'actions_get') return set;
    if (op === 'actions_save') {
      if (args.revision !== set.revision) throw new ApiError('conflict', 'The buttons changed since you opened Settings.');
      set = { revision: set.revision + 1, seedVersion: set.seedVersion, buttons: args.buttons as ActionButton[] };
      return set;
    }
    if (op === 'chat_search') return fakeSearch(String(args.query ?? ''), ctx.call);
    throw new ApiError('refused', `fake-leaf has no op ${op}`);
  };
  return { prefixes: ['actions_', 'chat_search'], handler };
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
