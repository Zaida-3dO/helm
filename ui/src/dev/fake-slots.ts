// The dev fake host's run-slot queue and usage governor ops. A prompt containing "wait for slot"
// in an idle chat is held as a send waiting for a free slot (the fake host calls `holdForSlot`);
// Start now runs it, Cancel ends it. `?slots=held` in the page URL makes the usage hold the reason.
//
// Dev builds only. fakeHost.ts imports this file, so this file only imports types from it.

import type { FakeOpHandler } from './fakeHost.ts';
import type { SlotQueueView, SlotSettings, SlotWait } from '../composer/slotQueue.ts';

const held = typeof location !== 'undefined' && new URLSearchParams(location.search).get('slots') === 'held';
let settings: SlotSettings = { enabled: true, lowRunningChats: 3 };
const waits: SlotWait[] = [];
let emitNow: ((payload: unknown) => void) | null = null;
let busyChats: () => string[] = () => [];

export function isSlotWaitPrompt(text: string): boolean {
  return /wait for slot/i.test(text);
}

/** Register a held send; the fake host keeps the submission itself. */
export function holdForSlot(chatId: string, submissionId: string): void {
  waits.push({ chatId, submissionId, position: waits.length + 1, since: Date.now(), head: false });
  publish();
}

function view(): SlotQueueView {
  const running = busyChats();
  return {
    settings,
    limit: settings.lowRunningChats,
    running,
    waiting: waits.map((w, i) => ({ ...w, position: i + 1 })),
    held: held ? 'Plan usage is near its limit, so automated starts wait.' : null,
  };
}

function publish(): SlotQueueView {
  const v = view();
  emitNow?.({ kind: 'resource-queue', ...v });
  return v;
}

export function slotsFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const handler: FakeOpHandler = (op, a, ctx) => {
    emitNow = ctx.emit;
    busyChats = () => (ctx.call('chat_list') as { id: string; status: string }[]).filter((c) => c.status === 'running' || c.status === 'starting' || c.status === 'stopping').map((c) => c.id);
    switch (op) {
      case 'resource_queue_snapshot':
        return view();
      case 'resource_queue_start_now':
      case 'resource_queue_cancel': {
        const i = waits.findIndex((w) => w.submissionId === a.submissionId);
        if (i >= 0) {
          const [w] = waits.splice(i, 1);
          ctx.call('dev_release_wait', { chatId: w!.chatId, submissionId: w!.submissionId, start: op === 'resource_queue_start_now' });
        }
        return publish();
      }
      case 'run_slots_settings':
        if (a.settings) settings = { ...settings, ...(a.settings as Partial<SlotSettings>) };
        return publish();
      case 'governor_state':
        return { settings: governor, defaults: governor, band: held ? 'windDown' : 'surplus', usedPct: held ? 82 : 20, since: null, told: 0, stopped: 0 };
      case 'governor_settings':
        governor = { ...governor, ...(a.settings as object) } as typeof governor;
        return { settings: governor, defaults: governor, band: 'surplus', usedPct: 20, since: null, told: 0, stopped: 0 };
      default:
        throw { code: 'refused', message: 'This operation is not available here.' };
    }
  };
  return { prefixes: ['resource_queue_', 'run_slots_', 'governor_'], handler };
}

let governor = { enabled: true, selectivePct: 70, windDownPct: 85, stopPct: 95, atStop: 'stop' as 'stop' | 'wrapUp' };
