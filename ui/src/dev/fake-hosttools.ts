// Dev fake for the host-tools ops (consent cards, settings Undo), so the Allow cards can be seen
// in a plain browser. Every chat starts with two pending cards: an Allow request whose reason
// carries a right-to-left override and a zero-width space, to show the inert rendering, and a
// staged-settings card that Helm applies on Approve.

//
// It also lets the dev console play the two tools that ask the PC showing a chat to act, as rows
// that arrive live (open the chat first, so the cards above have been read):
//   __helmFakeHostTools.openPanel('<chatId>', 'C:/Users/me/notes/plan.md', 3)
//   __helmFakeHostTools.openPage('<chatId>', '<pin id>', 'Progress')
//   __helmFakeHostTools.forward('<chatId>', 5173)        (acts on a chat of another device)

import type { FakeOpHandler, FakeOpContext } from './fakeHost.ts';
import { row } from './fixtures.ts';

/** The fake host's event channel, kept from the last op this fake answered. */
let emitLive: FakeOpContext['emit'] | null = null;

/** A finished host-tool row, sent as a live `message` event. */
function liveToolRow(chatId: string, tool: string, result: unknown): void {
  if (!emitLive) {
    console.warn('Open a chat first: the fake host has not asked for its cards yet.');
    return;
  }
  const id = Date.now();
  const r = row({ id, chatId, role: 'toolResult', toolName: `mcp__helm__${tool}`, toolCallId: `fake-${id}`, text: JSON.stringify(result), timestamp: id });
  emitLive({ kind: 'message', chatId, revision: id, rows: [r] });
}

if (typeof window !== 'undefined') {
  (window as unknown as { __helmFakeHostTools?: unknown }).__helmFakeHostTools = {
    openPanel: (chatId: string, path: string, line?: number) =>
      liveToolRow(chatId, 'app_open_panel', { opened: { kind: 'file', path, ...(line ? { line } : {}) }, note: 'Shown in the side panel.' }),
    openPage: (chatId: string, pageId: string, title = 'Page') =>
      liveToolRow(chatId, 'app_open_panel', { opened: { kind: 'page', pageId, path: '', title }, link: `helm://pages/${pageId}`, note: 'Shown in the side panel.' }),
    forward: (chatId: string, port: number, samePc = false) =>
      liveToolRow(chatId, 'app_forward_port', { forward: { port, title: 'Dev server' }, typedOn: samePc ? null : 'Laptop', samePc, note: 'Offered.' }),
  };
}

interface FakeRequest {
  id: string;
  chatId: string;
  state: string;
}

export function hosttoolsFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const decided = new Map<string, FakeRequest>();
  const card = (chatId: string) => ({
    id: `fake-allow-${chatId}`,
    chatId,
    kind: 'test_action',
    label: 'Run a test action',
    summary: 'Run the test action demo',
    reason: 'The user asked me to run the demo.\nHidden: ‮evil​ text',
    evidence: '',
    state: 'pending',
    grantMinutes: 30,
    createdAt: Date.now(),
    expiresAt: Date.now() + 86_400_000,
    decidedAt: null,
    grantExpiresAt: null,
    note: null,
    appliesOnAllow: false,
    outcome: null,
    decidedVia: null,
    picture: null,
  });
  // A staged-settings card: Helm applies it on Approve, one row per change.
  const stagedCard = (chatId: string) => ({
    ...card(chatId),
    id: `fake-staged-${chatId}`,
    kind: 'staged_settings',
    label: 'Change settings',
    summary: [
      'Change 3 settings',
      'Update folder: none → \\\\server\\share\\helm\\dist',
      'Let other devices use chats on this PC: on → off',
      'Pair with the hub at 192.168.1.10:8000 as Sam on Laptop, using the one-time code in \\\\server\\share\\pair.txt',
    ].join('\n'),
    reason: 'The user asked me to set up the update folder and pair this PC.',
    appliesOnAllow: true,
  });
  const cardFor = (id: string) => {
    const chatId = id.replace(/^fake-(allow|staged)-/, '');
    return id.startsWith('fake-staged-') ? stagedCard(chatId) : card(chatId);
  };
  // Two sample changes so Settings → General has something to show and undo.
  const changes = [
    { changeId: 2, chatId: 'fake', key: 'compactSidebar', before: null, after: true, at: Date.now() - 3_600_000, undone: false },
    { changeId: 1, chatId: 'fake', key: 'zoomLevel', before: 100, after: 120, at: Date.now() - 86_400_000, undone: false },
  ];
  const handler: FakeOpHandler = (op, args, ctx) => {
    emitLive = ctx.emit;
    switch (op) {
      case 'consent_list': {
        const chatId = String(args.chatId ?? '');
        const open = [card(chatId), stagedCard(chatId)].filter((c) => !decided.has(c.id));
        return { pending: open, decided: [] };
      }
      case 'consent_decide': {
        const id = String(args.requestId ?? '');
        if (decided.has(id)) throw { code: 'conflict', message: 'This request was already answered or has expired.' };
        const c = cardFor(id);
        decided.set(id, { id, chatId: c.chatId, state: args.decision === 'allow' ? 'allowed' : 'denied' });
        ctx.emit({ kind: 'consent', chatId: c.chatId });
        return { ...c, state: decided.get(id)?.state };
      }
      case 'settings_undo': {
        for (const id of (args.changeIds as number[]) ?? []) {
          const c = changes.find((x) => x.changeId === id);
          if (c) c.undone = true;
        }
        return { restored: {} };
      }
      case 'settings_changes_list':
        return { changes };
      case 'settings_all_set':
        return null;
      default:
        return undefined;
    }
  };
  return { prefixes: ['consent_', 'settings_undo', 'settings_changes_list', 'settings_all_set'], handler };
}
