// Dev fake host: the gatekeeper's ops over an in-memory audit store, seeded with a little
// history so the review list, the marks and the History can be checked by eye, and the working
// mode's ops over an in-memory mode (the app's, plus boards set from the chip or Settings).
// Changing an answer or a mark updates the store the same way the real one would, and a `gate`
// or `workmode` event rides every change.

import type { FakeOpHandler } from './fakeHost.ts';
import type { GateAsk } from '../gatekeeper/types.ts';

const H = 3_600_000;

let seq = 0;
function ask(p: Partial<GateAsk> & Pick<GateAsk, 'chatId' | 'chatTitle' | 'question' | 'verdict' | 'state'>): GateAsk {
  seq += 1;
  return {
    id: `ask-${seq}`,
    profileId: 'p',
    channel: 'question',
    requestRef: `req-${seq}`,
    askedAt: Date.now() - 2 * H,
    review: 'pending',
    ...p,
  };
}

const rows: GateAsk[] = [
  ask({
    chatId: 'notes-app',
    chatTitle: 'Notes app cleanup',
    question: 'Which export format for the notes?',
    verdict: 'decided',
    state: 'open',
    decidedBy: 'claude',
    model: 'sonnet',
    confidence: 0.9,
    risk: 'low',
    reason: 'CSV opens straight in a spreadsheet and the import accepts it back',
    choice: 'CSV',
    tookMs: 2100,
    askedAt: Date.now() - 2 * H,
  }),
  ask({
    chatId: 'notes-app',
    chatTitle: 'Notes app cleanup',
    question: 'Where should the export write?',
    verdict: 'decided',
    state: 'open',
    decidedBy: 'claude',
    model: 'sonnet',
    confidence: 0.82,
    risk: 'low',
    reason: 'next to the source file is where the sample exports already are',
    choice: 'Next to the file',
    tookMs: 1750,
    askedAt: Date.now() - 5 * H,
  }),
  ask({
    chatId: 'report-script',
    chatTitle: 'Weekly report script',
    question: 'Which branch should the fix go on?',
    verdict: 'decided',
    state: 'kept',
    decidedBy: 'claude',
    model: 'sonnet',
    confidence: 0.78,
    risk: 'low',
    reason: 'the chat already worked on a fix branch',
    choice: 'fix/weekly-total',
    tookMs: 1900,
    askedAt: Date.now() - 9 * 24 * H,
  }),
  ask({
    chatId: 'photos-import',
    chatTitle: 'Photo import helper',
    question: 'Rename the copied files by date?',
    verdict: 'decided',
    state: 'vetoed',
    review: 'should_have_asked',
    reviewNote: 'the old names were the camera roll order, which I rely on',
    decidedBy: 'claude',
    model: 'sonnet',
    confidence: 0.71,
    risk: 'medium',
    reason: 'date names sort themselves',
    choice: 'Yes, rename',
    answer: 'No, keep the names',
    answeredVia: 'chat',
    answeredAt: Date.now() - 3 * H,
    tookMs: 2400,
    askedAt: Date.now() - 4 * H,
  }),
  ask({
    chatId: 'backups',
    chatTitle: 'Backup tidy-up',
    question: 'May I delete the old backup copies?',
    verdict: 'escalated',
    state: 'open',
    risk: 'high',
    brief: {
      problem: 'The chat wants to delete twelve old backup copies to free the folder. Deleting them cannot be undone from the chat.',
      options: [
        { label: 'Delete them', means: 'Frees the space for good.', pros: 'Done in one step', cons: 'Nothing can bring them back', cost: 'A few seconds' },
        { label: 'Move to the archive folder', means: 'Out of the way, still there.', pros: 'Reversible', cons: 'Keeps the space used', cost: 'About a minute' },
      ],
      recommendation: 'Move to the archive folder',
      why: 'Nothing is lost and the folder is just as tidy.',
    },
    reason: 'deleting data cannot be undone',
    tookMs: 2600,
    askedAt: Date.now() - 40 * 60_000,
  }),
  ask({
    chatId: 'site-redesign',
    chatTitle: 'Portfolio site redesign',
    question: 'Push the new header to the live site now?',
    verdict: 'escalated',
    state: 'answered',
    answer: 'Not yet, wait for the review',
    answeredVia: 'helm',
    answeredAt: Date.now() - 26 * H,
    reason: 'touches anything live',
    tookMs: 2200,
    askedAt: Date.now() - 27 * H,
  }),
  ask({
    chatId: 'notes-app',
    chatTitle: 'Notes app cleanup',
    question: 'Which icon fits the new toolbar?',
    verdict: 'failed',
    state: 'withdrawn',
    reason: 'the judgement could not be reached in time',
    askedAt: Date.now() - 26 * H,
  }),
  ask({
    chatId: 'backups',
    chatTitle: 'Backup tidy-up',
    question: 'Which credentials file should be used for the upload?',
    verdict: 'passed',
    state: 'answered',
    passReason: 'the question asks for a secret',
    answeredVia: 'helm',
    answeredAt: Date.now() - 50 * H,
    askedAt: Date.now() - 50 * H,
  }),
];

let settings = {
  enabled: true,
  askFrequency: 'balanced',
  policy: null as string | null,
  defaultPolicy:
    'Only ask the user when it truly needs them. Where a reasonable default exists and staying easy to undo, take it and carry on. Always hand the choice to the user when it cannot be undone, costs money, sends anything to other people, touches credentials, or touches anything live or in production. Prefer the choice that keeps the work moving and stays easy to reverse.',
  model: 'sonnet',
  bar: 0.75,
  paused: false,
  pausedUntil: null as number | null,
};

/** A decided row from the fake turn script, so a "gate" prompt's decision shows up in the
 *  review list like a real one. */
export function fakeGateAddDecided(chatId: string, chatTitle: string, question: string, choice: string, reason: string): GateAsk {
  const r = ask({ chatId, chatTitle, question, verdict: 'decided', state: 'open', decidedBy: 'claude', model: settings.model, confidence: 0.85, risk: 'low', reason, choice, tookMs: 2000, askedAt: Date.now() });
  rows.unshift(r);
  return r;
}

/** A checked stop from the fake turn script: a nudge reads as decided (its `choice` is the
 *  default Helm took), a real stop as escalated with its class. */
export function fakeGateAddStop(chatId: string, chatTitle: string, question: string, cls: 'blocker' | 'limit' | 'failure' | 'asked', choice?: string, reason?: string): GateAsk {
  const r = ask({
    chatId,
    chatTitle,
    question,
    channel: 'stop',
    verdict: choice ? 'decided' : 'escalated',
    state: 'open',
    decidedBy: choice ? 'claude' : undefined,
    model: choice ? settings.model : undefined,
    confidence: 0.85,
    risk: choice ? 'low' : 'high',
    reason,
    choice,
    stopClass: choice ? undefined : cls,
    tookMs: 1800,
    askedAt: Date.now(),
  });
  rows.unshift(r);
  return r;
}

// The working mode: the app default, and boards with a choice of their own. The fake chats are
// not shown under boards, so the chip reads the app's mode; Settings lists what is set.
let appMode: 'run' | 'check_in' = 'check_in';
const boardModes = new Map<string, { name: string; mode: 'run' | 'check_in' }>();

function modeView() {
  const decided = rows.filter((r) => r.verdict === 'decided' && r.review === 'pending' && (r.state === 'open' || r.state === 'kept')).length;
  const needsYou = rows.filter((r) => r.verdict === 'escalated' && r.state === 'open').length;
  return {
    app: appMode,
    effective: appMode,
    source: 'app' as const,
    overrides: [...boardModes.entries()].map(([id, b]) => ({ id, name: b.name, mode: b.mode })),
    counts: { decided, needsYou },
  };
}

export function gatekeeperFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  return {
    prefixes: ['gate_', 'workmode_'],
    handler: (op, a, ctx) => {
      if (op === 'workmode_get') return modeView();
      if (op === 'workmode_set') {
        const scope = String(a.scope ?? 'app');
        const mode = a.mode === null || a.mode === undefined ? null : String(a.mode);
        if (scope === 'board') {
          const id = String(a.boardId ?? '');
          if (!id) throw new Error('not_found: That is not a board.');
          if (mode === null) boardModes.delete(id);
          else if (mode === 'run' || mode === 'check_in') boardModes.set(id, { name: `Board ${id}`, mode });
          else throw new Error('The mode must be run or check in.');
        } else if (mode === 'run' || mode === 'check_in') {
          appMode = mode;
        } else if (mode === null) {
          appMode = 'check_in';
        } else {
          throw new Error('The mode must be run or check in.');
        }
        ctx.emit({ kind: 'workmode', scope, boardId: String(a.boardId ?? '') });
        return modeView();
      }
      if (op === 'gate_list') {
        const filter = String(a.filter ?? 'all');
        const query = String(a.query ?? '').trim().toLowerCase();
        const limit = Math.min(500, Math.max(1, Number(a.limit ?? 200)));
        const keep = rows.filter((r) => {
          if (filter === 'decided' && r.verdict !== 'decided') return false;
          if (filter === 'asked' && r.verdict !== 'escalated') return false;
          if (filter === 'failed' && r.verdict !== 'failed') return false;
          if (filter === 'review' && !(r.verdict === 'decided' && r.state === 'open' && r.review === 'pending')) return false;
          if (query && !`${r.question} ${r.chatTitle} ${r.choice ?? ''} ${r.reason ?? ''}`.toLowerCase().includes(query)) return false;
          return true;
        });
        return { rows: keep.slice(0, limit) };
      }
      if (op === 'gate_review') {
        const r = rows.find((x) => x.id === a.id);
        if (!r) throw new Error('no such record');
        const review = String(a.review);
        if (!['fine', 'should_have_asked', 'didnt_need_me'].includes(review)) throw new Error('unknown mark');
        r.review = review;
        r.reviewNote = a.note ? String(a.note) : undefined;
        return r;
      }
      if (op === 'gate_change_answer') {
        const r = rows.find((x) => x.id === a.id);
        if (!r) throw new Error('no such record');
        if (r.verdict !== 'decided' || (r.state !== 'open' && r.state !== 'kept')) throw new Error('only a decided answer can be changed');
        r.state = 'vetoed';
        r.answer = String(a.answer);
        r.answeredVia = 'helm';
        r.answeredAt = Date.now();
        return r;
      }
      if (op === 'gate_settings_get') return { ...settings };
      if (op === 'gate_settings_save') {
        const s = a.settings as Record<string, unknown>;
        settings = {
          ...settings,
          enabled: !!s.enabled,
          askFrequency: ['less', 'balanced', 'more'].includes(String(s.askFrequency)) ? String(s.askFrequency) : 'balanced',
          policy: typeof s.policy === 'string' && s.policy.trim() ? s.policy : null,
          model: String(s.model ?? 'sonnet'),
        };
        settings.bar = settings.askFrequency === 'less' ? 0.6 : settings.askFrequency === 'more' ? 0.9 : 0.75;
        return { ...settings };
      }
      throw new Error(`unknown op ${op}`);
    },
  };
}
