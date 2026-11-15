// Dev fake for the host-tools ops (consent cards, settings Undo), so the Allow cards can be seen
// in a plain browser. Every chat starts with two pending cards: an Allow request whose reason
// carries a right-to-left override and a zero-width space, to show the inert rendering, and a
// staged-settings card that Helm applies on Approve.

import type { FakeOpHandler } from './fakeHost.ts';

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
