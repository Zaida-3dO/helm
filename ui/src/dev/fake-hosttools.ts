// Dev fake for the host-tools ops (consent cards, settings Undo), so the Allow cards can be seen
// in a plain browser. Every chat starts with one pending Allow request whose reason carries a
// right-to-left override and a zero-width space, to show the inert rendering.

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
  });
  const handler: FakeOpHandler = (op, args, ctx) => {
    switch (op) {
      case 'consent_list': {
        const chatId = String(args.chatId ?? '');
        const c = card(chatId);
        return { pending: decided.has(c.id) ? [] : [c], decided: [] };
      }
      case 'consent_decide': {
        const id = String(args.requestId ?? '');
        if (decided.has(id)) throw { code: 'conflict', message: 'This request was already answered or has expired.' };
        const chatId = id.replace(/^fake-allow-/, '');
        decided.set(id, { id, chatId, state: args.decision === 'allow' ? 'allowed' : 'denied' });
        ctx.emit({ kind: 'consent', chatId });
        return { ...card(chatId), state: decided.get(id)?.state };
      }
      case 'settings_undo':
        return { restored: {} };
      case 'settings_changes_list':
        return { changes: [] };
      default:
        return undefined;
    }
  };
  return { prefixes: ['consent_', 'settings_undo', 'settings_changes_list'], handler };
}
